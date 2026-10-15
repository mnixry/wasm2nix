use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

use cranelift_codegen::ir::{self, ArgumentPurpose, Block, Inst, InstructionData, Opcode};
use snafu::prelude::*;

use super::{
    error::{
        EmptyBlockSnafu, MissingEntryBlockSnafu, MissingVmContextSnafu, Result,
        UnsupportedOpcodeSnafu,
    },
    inst_lower::{Force, Step, lower_inst, truthy_value_expr, value_expr, value_name},
    lambda_chain, ops,
};
use crate::{
    nix::{AttrBind, AttrKey, BinOp, Expr, LetBind, nix},
    translator::NixModule,
};

#[allow(clippy::too_many_lines)]
pub(super) fn lower_function(
    module: &NixModule, func_index: u32, func: &ir::Function,
) -> Result<Expr> {
    let entry = func
        .layout
        .entry_block()
        .context(MissingEntryBlockSnafu { func_index })?;

    let vmctx = func
        .special_param(ArgumentPurpose::VMContext)
        .context(MissingVmContextSnafu { func_index })?;

    // Function parameters: entry block params excluding vmctx, then `st`.
    let mut params: Vec<String> = func
        .dfg
        .block_params(entry)
        .iter()
        .copied()
        .filter(|v| *v != vmctx)
        .map(value_name)
        .collect();
    params.push("st".into());

    let cfg = ControlFlow::analyze(func, entry, vmctx);

    // Blocks call their forward successors directly and return either a done
    // record or, on a loop back edge, a jump record for the trampoline below.
    let lowering = BlockLowering {
        module,
        func,
        vmctx,
        cfg: &cfg,
        spilled: RefCell::new(Vec::new()),
    };
    let blocks_attrset = Expr::attrset(true, lowering.lower_blocks(entry)?);

    let mut entry_args: Vec<Expr> = func
        .dfg
        .block_params(entry)
        .iter()
        .copied()
        .filter(|v| *v != vmctx)
        .map(|v| Expr::ident(value_name(v)))
        .collect();
    entry_args.extend(
        cfg.free_values
            .get(&entry)
            .into_iter()
            .flat_map(|values| values.iter())
            .map(|v| Expr::ident(value_name(*v))),
    );

    if cfg.back_edges.is_empty() {
        entry_args.push(nix!(st));
        let entry_call = Expr::app(
            Expr::select(nix!(blocks), AttrKey::ident(block_name(entry))),
            entry_args,
        );
        return Ok(lambda_chain(
            &params,
            nix!(let
            blocks = #{blocks_attrset};
        in #{entry_call}),
        ));
    }

    let entry_with_key = lower_jump_record_with_key(entry, entry_args, nix!(st), 0);
    let apply_args = nix!(func: args: builtins."foldl'"(f: arg: f(arg), func, args));
    let apply_block = nix!(block: args: blockSt:
        (applyArgs(builtins.getAttr(block, blocks), args))(blockSt)
    );
    let force_step = nix!(nextKey == #{nix!(nextKey / 1024)} * 1024);
    let step_next = nix!(let
        next = applyBlock(current.block, current.args, current.st);
        nextKey = current.key + 1;
        nextKeyed = merge(next, { key = nextKey; });
        forceStep = #{force_step};
    in if forceStep then builtins.deepSeq(nextKeyed, nextKeyed) else nextKeyed);
    let step_item =
        nix!(current: if current.tag == "done" then [] else #{Expr::list(vec![step_next])});
    let trace_expr = nix!(builtins.genericClosure({
        startSet = [#{entry_with_key}];
        operator = stepItem;
    }));
    let result_index = nix!(builtins.length(trace) - 1);
    let body = nix!(let
        blocks = #{blocks_attrset};
        applyArgs = #{apply_args};
        applyBlock = #{apply_block};
        stepItem = #{step_item};
        trace = #{trace_expr};
        result = builtins.elemAt(trace, #{result_index});
    in {
        st = result.st;
        results = result.results;
    });
    Ok(lambda_chain(&params, body))
}

fn block_free_values(func: &ir::Function, vmctx: ir::Value) -> BTreeMap<Block, Vec<ir::Value>> {
    let mut result = BTreeMap::new();
    let mut defined_by_block = BTreeMap::new();
    let mut successors_by_block = BTreeMap::new();

    for block in func.layout.blocks() {
        let mut defined = func
            .dfg
            .block_params(block)
            .iter()
            .copied()
            .map(|v| func.dfg.resolve_aliases(v))
            .filter(|v| *v != vmctx)
            .collect::<Vec<_>>();
        let mut free = Vec::new();

        for inst in func.layout.block_insts(block) {
            for value in func.dfg.inst_values(inst) {
                push_free_value(func, value, vmctx, &defined, &mut free);
            }

            for block_call in branch_calls(func, inst) {
                successors_by_block
                    .entry(block)
                    .or_insert_with(Vec::new)
                    .push(block_call.block(&func.dfg.value_lists));
                for value in block_call.args_slice(&func.dfg.value_lists) {
                    push_free_value(func, *value, vmctx, &defined, &mut free);
                }
            }
            defined.extend(
                func.dfg
                    .inst_results(inst)
                    .iter()
                    .copied()
                    .map(|v| func.dfg.resolve_aliases(v)),
            );
        }

        defined_by_block.insert(block, defined);
        result.insert(block, free);
    }

    let mut changed = true;
    while changed {
        changed = false;

        for block in func.layout.blocks() {
            let defined = defined_by_block.get(&block).cloned().unwrap_or_default();
            let successors = successors_by_block.get(&block).cloned().unwrap_or_default();
            let mut free = result.get(&block).cloned().unwrap_or_default();

            for successor in successors {
                let successor_free = result.get(&successor).cloned().unwrap_or_default();
                for value in successor_free {
                    let before = free.len();
                    push_free_value(func, value, vmctx, &defined, &mut free);
                    changed |= free.len() != before;
                }
            }

            result.insert(block, free);
        }
    }

    result
}

/// The block calls an instruction branches through, if it is a branch.
fn branch_calls(func: &ir::Function, inst: Inst) -> Vec<ir::BlockCall> {
    match &func.dfg.insts[inst] {
        InstructionData::Jump { destination, .. } => vec![*destination],
        InstructionData::Brif { blocks, .. } => blocks.to_vec(),
        InstructionData::BranchTable { table, .. } => {
            let table_data = &func.dfg.jump_tables[*table];
            std::iter::once(table_data.default_block())
                .chain(table_data.as_slice().iter().copied())
                .collect()
        }
        _ => Vec::new(),
    }
}

struct ControlFlow {
    /// Values each block uses without defining, passed as extra parameters.
    free_values: BTreeMap<Block, Vec<ir::Value>>,
    /// Edges into a block that is still on the depth-first search path. Wasm
    /// control flow is reducible, so these are exactly the loop back edges.
    back_edges: BTreeSet<(Block, Block)>,
    /// Number of branch edges into each block.
    in_edges: BTreeMap<Block, usize>,
}

impl ControlFlow {
    fn analyze(func: &ir::Function, entry: Block, vmctx: ir::Value) -> Self {
        let successors = |block: Block| {
            func.layout
                .last_inst(block)
                .map(|inst| branch_calls(func, inst))
                .unwrap_or_default()
                .into_iter()
                .map(|call| call.block(&func.dfg.value_lists))
                .collect::<Vec<_>>()
        };

        let mut back_edges = BTreeSet::new();
        let mut on_path = BTreeSet::from([entry]);
        let mut visited = BTreeSet::from([entry]);
        let mut stack = vec![(entry, successors(entry).into_iter())];
        while let Some((block, pending)) = stack.last_mut() {
            let block = *block;
            if let Some(next) = pending.next() {
                if on_path.contains(&next) {
                    back_edges.insert((block, next));
                } else if visited.insert(next) {
                    on_path.insert(next);
                    stack.push((next, successors(next).into_iter()));
                }
            } else {
                on_path.remove(&block);
                stack.pop();
            }
        }

        let mut in_edges = BTreeMap::new();
        for block in func.layout.blocks() {
            for target in successors(block) {
                *in_edges.entry(target).or_insert(0) += 1;
            }
        }

        Self {
            free_values: block_free_values(func, vmctx),
            back_edges,
            in_edges,
        }
    }

    fn is_inlinable(&self, block: Block) -> bool {
        self.in_edges.get(&block) == Some(&1)
    }
}

fn push_free_value(
    func: &ir::Function, value: ir::Value, vmctx: ir::Value, defined: &[ir::Value],
    free: &mut Vec<ir::Value>,
) {
    let value = func.dfg.resolve_aliases(value);
    if value != vmctx && !defined.contains(&value) && !free.contains(&value) {
        free.push(value);
    }
}

/// Single-edge blocks nest at most this deep before they are emitted as
/// separate block functions, which bounds recursion in the lowering and in
/// the printer.
const MAX_INLINE_DEPTH: usize = 32;

struct BlockLowering<'a> {
    module: &'a NixModule,
    func: &'a ir::Function,
    vmctx: ir::Value,
    cfg: &'a ControlFlow,
    /// Single-edge blocks that were called instead of inlined because the
    /// nesting limit was reached.
    spilled: RefCell<Vec<Block>>,
}

impl BlockLowering<'_> {
    /// Lowers every block that needs its own function: the entry, blocks with
    /// several incoming edges, loop headers and blocks spilled at the nesting
    /// limit. Every other block is inlined into its only predecessor.
    fn lower_blocks(&self, entry: Block) -> Result<Vec<(AttrKey, Expr)>> {
        let mut pending: Vec<Block> = self
            .func
            .layout
            .blocks()
            .filter(|block| *block == entry || !self.cfg.is_inlinable(*block))
            .collect();
        pending.reverse();

        let mut emitted = BTreeSet::new();
        let mut binds = Vec::new();
        while let Some(block) = pending.pop() {
            if !emitted.insert(block) {
                continue;
            }
            binds.push((AttrKey::ident(block_name(block)), self.lower_block(block)?));
            pending.extend(self.spilled.borrow_mut().drain(..).rev());
        }
        Ok(binds)
    }

    fn lower_block(&self, block: Block) -> Result<Expr> {
        let mut params = self.block_params(block);
        params.extend(
            self.cfg
                .free_values
                .get(&block)
                .into_iter()
                .flat_map(|values| values.iter())
                .map(|v| value_name(*v)),
        );
        params.push("st".into());

        let body = self.lower_block_body(block, 0)?;
        Ok(lambda_chain(&params, body))
    }

    /// Block parameters, excluding vmctx.
    fn block_params(&self, block: Block) -> Vec<String> {
        self.func
            .dfg
            .block_params(block)
            .iter()
            .copied()
            .filter(|v| *v != self.vmctx)
            .map(value_name)
            .collect()
    }

    fn lower_block_body(&self, block: Block, depth: usize) -> Result<Expr> {
        let mut insts: Vec<Inst> = self.func.layout.block_insts(block).collect();
        ensure!(
            !insts.is_empty(),
            EmptyBlockSnafu {
                block: block.as_u32()
            }
        );

        let term = insts.pop().context(EmptyBlockSnafu {
            block: block.as_u32(),
        })?;

        let mut steps: Vec<Step> = Vec::new();
        let mut st_name = "st".to_string();

        for inst in insts {
            let lowered = lower_inst(self.module, self.func, inst, &st_name, self.vmctx)?;
            if let Some(step) = lowered.step {
                steps.push(step);
            }
            if let Some(new_st) = lowered.next_state {
                st_name = new_st;
            }
        }

        let expr = self.lower_terminator(block, term, &st_name, depth)?;
        let lowered = inline_single_use_values(steps, expr);
        let conds = lowered.forces.into_iter().map(Force::condition).collect();
        let expr = Expr::asserts(conds, lowered.body);

        Ok(Expr::let_binds(lowered.binds, expr))
    }

    fn value_expr(&self, v: ir::Value, st_name: &str) -> Expr {
        value_expr(self.func, v, st_name, self.vmctx)
    }

    fn lower_terminator(
        &self, block: Block, inst: Inst, st_name: &str, depth: usize,
    ) -> Result<Expr> {
        let func = self.func;
        let data = &func.dfg.insts[inst];
        let op = data.opcode();

        Ok(match (op, data) {
            (Opcode::Return, InstructionData::MultiAry { args, .. }) => {
                let rvs = args
                    .as_slice(&func.dfg.value_lists)
                    .iter()
                    .map(|v| self.value_expr(*v, st_name))
                    .collect::<Vec<_>>();
                lower_done_record(Expr::ident(st_name), rvs)
            }

            (Opcode::Jump, InstructionData::Jump { destination, .. }) => {
                self.lower_block_call(block, *destination, st_name, depth)?
            }

            (Opcode::Brif, InstructionData::Brif { blocks, arg, .. }) => {
                let cond = truthy_value_expr(func, *arg, st_name, self.vmctx);
                let then_call = self.lower_block_call(block, blocks[0], st_name, depth)?;
                let else_call = self.lower_block_call(block, blocks[1], st_name, depth)?;
                nix!(if #{cond} then #{then_call} else #{else_call})
            }

            (Opcode::BrTable, InstructionData::BranchTable { arg, table, .. }) => {
                self.lower_branch_table(block, inst, *arg, *table, st_name, depth)?
            }

            (Opcode::Trap, _) => {
                let trap_code = data
                    .trap_code()
                    .map_or_else(|| "?".into(), |code| code.to_string());
                nix!(throw(#{format!("wasm2nix: trap {trap_code}")}))
            }

            (Opcode::Trapz, InstructionData::CondTrap { arg, code, .. }) => {
                let c = truthy_value_expr(func, *arg, st_name, self.vmctx);
                // trapz: if c is false then trap.
                nix!(if #{c}
                    then #{lower_done_record(Expr::ident(st_name), vec![])}
                    else throw(#{format!("wasm2nix: trapz {code}")}))
            }

            (Opcode::Trapnz, InstructionData::CondTrap { arg, code, .. }) => {
                let c = truthy_value_expr(func, *arg, st_name, self.vmctx);
                let ok = lower_done_record(Expr::ident(st_name), vec![]);
                // trapnz: if c is true then trap.
                nix!(if #{c} then throw(#{format!("wasm2nix: trapnz {code}")}) else #{ok})
            }

            _ => {
                return UnsupportedOpcodeSnafu {
                    opcode: op.to_string(),
                }
                .fail();
            }
        })
    }

    fn lower_branch_table(
        &self, block: Block, inst: Inst, arg: ir::Value, table: ir::JumpTable, st_name: &str,
        depth: usize,
    ) -> Result<Expr> {
        let index_name = format!("br_table_index_{}", inst.as_u32());
        let targets_name = format!("br_table_targets_{}", inst.as_u32());
        let table_data = &self.func.dfg.jump_tables[table];
        let targets = table_data
            .as_slice()
            .iter()
            .map(|branch| self.lower_block_call(block, *branch, st_name, depth))
            .collect::<Result<Vec<_>>>()?;
        let target_count =
            i64::try_from(targets.len()).expect("branch table target count fits in i64");
        let default = self.lower_block_call(block, table_data.default_block(), st_name, depth)?;
        let selected = Expr::app(
            Expr::select(nix!(builtins), AttrKey::ident("elemAt")),
            vec![
                Expr::ident(targets_name.clone()),
                Expr::ident(index_name.clone()),
            ],
        );
        let in_range = nix!(
            #{Expr::ident(index_name.clone())} >= 0
                && #{Expr::ident(index_name.clone())} < #{target_count}
        );

        Ok(Expr::let_in(
            vec![
                (index_name, self.value_expr(arg, st_name)),
                (targets_name, Expr::list(targets)),
            ],
            nix!(if #{in_range} then #{selected} else #{default}),
        ))
    }

    /// Lowers a branch to `call`. A block with a single incoming edge is
    /// inlined with its parameters bound by `let`; its free values are already
    /// in scope because they are free in, or defined by, the predecessor.
    /// Other forward edges call the target block directly, and back edges
    /// return a jump record to the function's trampoline so that loops do not
    /// grow the Nix call stack.
    fn lower_block_call(
        &self, from: Block, call: ir::BlockCall, st_name: &str, depth: usize,
    ) -> Result<Expr> {
        let value_lists = &self.func.dfg.value_lists;
        let target = call.block(value_lists);
        let mut args = call
            .args_slice(value_lists)
            .iter()
            .map(|v| self.value_expr(*v, st_name))
            .collect::<Vec<_>>();

        if self.cfg.is_inlinable(target) && !self.cfg.back_edges.contains(&(from, target)) {
            if depth < MAX_INLINE_DEPTH {
                let mut binds = self
                    .block_params(target)
                    .into_iter()
                    .zip(args)
                    .collect::<Vec<_>>();
                if st_name != "st" {
                    binds.push(("st".into(), Expr::ident(st_name)));
                }
                return Ok(Expr::let_in(
                    binds,
                    self.lower_block_body(target, depth + 1)?,
                ));
            }
            self.spilled.borrow_mut().push(target);
        }

        args.extend(
            self.cfg
                .free_values
                .get(&target)
                .into_iter()
                .flat_map(|values| values.iter())
                .map(|v| self.value_expr(*v, st_name)),
        );
        if self.cfg.back_edges.contains(&(from, target)) {
            Ok(lower_jump_record(target, args, Expr::ident(st_name)))
        } else {
            args.push(Expr::ident(st_name));
            Ok(Expr::app(Expr::ident(block_name(target)), args))
        }
    }
}

struct LoweredBlockBody {
    binds: Vec<LetBind>,
    forces: Vec<Force>,
    body: Expr,
}

fn inline_single_use_values(steps: Vec<Step>, body: Expr) -> LoweredBlockBody {
    let mut binds = Vec::new();
    let mut forces = Vec::new();

    for step in steps {
        binds.extend(step.binds.into_iter().map(LetBind::from));
        if let Some(force) = step.force {
            forces.push(force);
        }
    }

    let mut body = body;
    let mut index = 0;
    while index < binds.len() {
        let name = binds[index].name.clone();
        let value = binds[index].value.clone();

        if !is_inline_candidate(&value) || count_ident_uses_in_binds(&binds[..index], &name) != 0 {
            index += 1;
            continue;
        }

        // Forcing a bound value is redundant once it is inlined; trap
        // conditions are ordinary use sites.
        let removable_force_count = forces
            .iter()
            .filter(|force| matches!(force, Force::Value(expr) if is_ident_named(expr, &name)))
            .count();
        if removable_force_count > 1 {
            index += 1;
            continue;
        }

        let use_count = count_ident_uses_in_binds(&binds[index + 1..], &name)
            + count_ident_uses_in_forces(&forces, &name)
            - removable_force_count
            + count_ident_uses(&body, &name);
        // List elements and attribute values stay unevaluated, so an expression
        // inlined into a jump record would grow a thunk chain across loop
        // iterations instead of being forced in this block.
        if use_count > 1 || is_used_lazily(&body, &name) {
            index += 1;
            continue;
        }

        if removable_force_count == 1 {
            remove_ident_force(&mut forces, &name);
        }
        if use_count == 1 {
            for bind in &mut binds[index + 1..] {
                substitute_ident(&mut bind.value, &name, &value);
            }
            for expr in forces.iter_mut().filter_map(Force::expr_mut) {
                substitute_ident(expr, &name, &value);
            }
            substitute_ident(&mut body, &name, &value);
        }
        binds.remove(index);
    }

    LoweredBlockBody {
        binds,
        forces,
        body,
    }
}

fn is_inline_candidate(expr: &Expr) -> bool {
    match expr {
        Expr::Ident(_) | Expr::Int(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Null => true,
        Expr::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            is_inline_candidate(cond)
                && is_inline_candidate(then_expr)
                && is_inline_candidate(else_expr)
        }
        // Division by a non-zero literal cannot trap.
        Expr::BinOp {
            op: BinOp::Div,
            lhs,
            rhs,
        } => {
            matches!(rhs.as_ref(), Expr::Int(divisor) if *divisor != 0) && is_inline_candidate(lhs)
        }
        Expr::BinOp { op, lhs, rhs } => {
            is_safe_inline_binop(*op) && is_inline_candidate(lhs) && is_inline_candidate(rhs)
        }
        Expr::UnOp { expr, .. } => is_inline_candidate(expr),
        Expr::Apply { func, args } => ops::is_bit_op(func) && args.iter().all(is_inline_candidate),
        Expr::Raw(_)
        | Expr::Path(_)
        | Expr::List(_)
        | Expr::AttrSet { .. }
        | Expr::Select { .. }
        | Expr::Lambda { .. }
        | Expr::LetIn { .. }
        | Expr::Assert { .. } => false,
    }
}

const fn is_safe_inline_binop(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Eq
            | BinOp::Neq
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
            | BinOp::And
            | BinOp::Or
    )
}

fn count_ident_uses_in_binds(binds: &[LetBind], name: &str) -> usize {
    binds
        .iter()
        .map(|bind| count_ident_uses(&bind.value, name))
        .sum()
}

fn count_ident_uses_in_exprs(exprs: &[Expr], name: &str) -> usize {
    exprs.iter().map(|expr| count_ident_uses(expr, name)).sum()
}

fn count_ident_uses_in_forces(forces: &[Force], name: &str) -> usize {
    forces
        .iter()
        .filter_map(Force::expr)
        .map(|expr| count_ident_uses(expr, name))
        .sum()
}

fn count_ident_uses(expr: &Expr, name: &str) -> usize {
    match expr {
        Expr::Ident(ident) => usize::from(ident == name),
        Expr::Raw(_) | Expr::Path(_) | Expr::Int(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Null => {
            0
        }
        Expr::List(items) => items.iter().map(|item| count_ident_uses(item, name)).sum(),
        Expr::AttrSet { recursive, binds } => {
            if *recursive && attr_binds_shadow(binds, name) {
                return 0;
            }
            binds
                .iter()
                .map(|bind| count_ident_uses(&bind.value, name))
                .sum()
        }
        Expr::Select { expr, .. } | Expr::UnOp { expr, .. } => count_ident_uses(expr, name),
        Expr::Lambda { param, body } => {
            if param == name {
                0
            } else {
                count_ident_uses(body, name)
            }
        }
        Expr::LetIn { binds, body } => {
            if binds.iter().any(|bind| bind.name == name) {
                return 0;
            }
            count_ident_uses_in_binds(binds, name) + count_ident_uses(body, name)
        }
        Expr::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            count_ident_uses(cond, name)
                + count_ident_uses(then_expr, name)
                + count_ident_uses(else_expr, name)
        }
        Expr::Apply { func, args } => {
            count_ident_uses(func, name)
                + args
                    .iter()
                    .map(|arg| count_ident_uses(arg, name))
                    .sum::<usize>()
        }
        Expr::BinOp { lhs, rhs, .. } => count_ident_uses(lhs, name) + count_ident_uses(rhs, name),
        Expr::Assert { conds, body } => {
            count_ident_uses_in_exprs(conds, name) + count_ident_uses(body, name)
        }
    }
}

fn is_used_lazily(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::List(items) => items.iter().any(|item| count_ident_uses(item, name) != 0),
        Expr::AttrSet { .. } => count_ident_uses(expr, name) != 0,
        Expr::Ident(_)
        | Expr::Raw(_)
        | Expr::Path(_)
        | Expr::Int(_)
        | Expr::Bool(_)
        | Expr::Str(_)
        | Expr::Null => false,
        Expr::Select { expr, .. } | Expr::UnOp { expr, .. } => is_used_lazily(expr, name),
        Expr::Lambda { param, body } => param != name && is_used_lazily(body, name),
        Expr::LetIn { binds, body } => {
            !binds.iter().any(|bind| bind.name == name)
                && (binds.iter().any(|bind| is_used_lazily(&bind.value, name))
                    || is_used_lazily(body, name))
        }
        Expr::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            is_used_lazily(cond, name)
                || is_used_lazily(then_expr, name)
                || is_used_lazily(else_expr, name)
        }
        Expr::Apply { func, args } => {
            is_used_lazily(func, name) || args.iter().any(|arg| is_used_lazily(arg, name))
        }
        Expr::BinOp { lhs, rhs, .. } => is_used_lazily(lhs, name) || is_used_lazily(rhs, name),
        Expr::Assert { conds, body } => {
            conds.iter().any(|cond| is_used_lazily(cond, name)) || is_used_lazily(body, name)
        }
    }
}

fn substitute_ident(expr: &mut Expr, name: &str, replacement: &Expr) {
    match expr {
        Expr::Ident(ident) if ident == name => {
            *expr = replacement.clone();
        }
        Expr::Ident(_)
        | Expr::Raw(_)
        | Expr::Path(_)
        | Expr::Int(_)
        | Expr::Bool(_)
        | Expr::Str(_)
        | Expr::Null => {}
        Expr::List(items) => {
            for item in items {
                substitute_ident(item, name, replacement);
            }
        }
        Expr::AttrSet { recursive, binds } => {
            if *recursive && attr_binds_shadow(binds, name) {
                return;
            }
            for bind in binds {
                substitute_ident(&mut bind.value, name, replacement);
            }
        }
        Expr::Select { expr, .. } | Expr::UnOp { expr, .. } => {
            substitute_ident(expr, name, replacement);
        }
        Expr::Lambda { param, body } => {
            if param != name {
                substitute_ident(body, name, replacement);
            }
        }
        Expr::LetIn { binds, body } => {
            if binds.iter().any(|bind| bind.name == name) {
                return;
            }
            for bind in binds {
                substitute_ident(&mut bind.value, name, replacement);
            }
            substitute_ident(body, name, replacement);
        }
        Expr::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            substitute_ident(cond, name, replacement);
            substitute_ident(then_expr, name, replacement);
            substitute_ident(else_expr, name, replacement);
        }
        Expr::Apply { func, args } => {
            substitute_ident(func, name, replacement);
            for arg in args {
                substitute_ident(arg, name, replacement);
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            substitute_ident(lhs, name, replacement);
            substitute_ident(rhs, name, replacement);
        }
        Expr::Assert { conds, body } => {
            for cond in conds {
                substitute_ident(cond, name, replacement);
            }
            substitute_ident(body, name, replacement);
        }
    }
}

fn attr_binds_shadow(binds: &[AttrBind], name: &str) -> bool {
    binds.iter().any(|bind| {
        bind.path
            .iter()
            .next()
            .is_some_and(|key| attr_key_matches(key, name))
    })
}

fn attr_key_matches(key: &AttrKey, name: &str) -> bool {
    match key {
        AttrKey::Ident(ident) | AttrKey::Str(ident) => ident == name,
    }
}

fn is_ident_named(expr: &Expr, name: &str) -> bool {
    matches!(expr, Expr::Ident(ident) if ident == name)
}

fn remove_ident_force(forces: &mut Vec<Force>, name: &str) {
    let mut removed = false;
    forces.retain(|force| {
        if !removed && matches!(force, Force::Value(expr) if is_ident_named(expr, name)) {
            removed = true;
            false
        } else {
            true
        }
    });
}

fn lower_jump_record(target: Block, args: Vec<Expr>, st: Expr) -> Expr {
    lower_jump_record_binds(target, args, st, None)
}

fn lower_jump_record_with_key(target: Block, args: Vec<Expr>, st: Expr, key: i64) -> Expr {
    lower_jump_record_binds(target, args, st, Some(key))
}

fn lower_jump_record_binds(target: Block, args: Vec<Expr>, st: Expr, key: Option<i64>) -> Expr {
    let block = block_name(target);
    let args = Expr::list(args);
    if let Some(key) = key {
        nix!({ tag = "jump"; block = #{block}; args = #{args}; st = #{st}; key = #{key}; })
    } else {
        nix!({ tag = "jump"; block = #{block}; args = #{args}; st = #{st}; })
    }
}

fn lower_done_record(st: Expr, results: Vec<Expr>) -> Expr {
    nix!({ tag = "done"; st = #{st}; results = #{Expr::list(results)}; })
}

fn block_name(b: Block) -> String {
    format!("b{}", b.as_u32())
}
