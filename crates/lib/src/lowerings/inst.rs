use std::collections::BTreeMap;

use cranelift_codegen::ir::{self, ArgumentPurpose, Block, Inst, InstructionData, Opcode};
use snafu::prelude::*;

use super::{
    error::{
        EmptyBlockSnafu, MissingEntryBlockSnafu, MissingVmContextSnafu, Result,
        UnsupportedOpcodeSnafu,
    },
    inst_lower::{Step, lower_inst, truthy_value_expr, value_expr, value_name},
    lambda_chain,
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

    let free_values = block_free_values(func, vmctx);

    // Blocks return either a jump record or a done record. The trampoline below
    // keeps Wasm loops from consuming the Nix evaluator call stack.
    let mut block_binds: Vec<(AttrKey, Expr)> = Vec::new();
    for block in func.layout.blocks() {
        let bname = block_name(block);
        let bexpr = lower_block(module, func, block, vmctx, &free_values)?;
        block_binds.push((AttrKey::ident(bname), bexpr));
    }
    let blocks_attrset = Expr::attrset(true, block_binds);

    let mut entry_args: Vec<Expr> = func
        .dfg
        .block_params(entry)
        .iter()
        .copied()
        .filter(|v| *v != vmctx)
        .map(|v| Expr::ident(value_name(v)))
        .collect();
    entry_args.extend(
        free_values
            .get(&entry)
            .into_iter()
            .flat_map(|values| values.iter())
            .map(|v| Expr::ident(value_name(*v))),
    );

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

            match &func.dfg.insts[inst] {
                InstructionData::Jump { destination, .. } => {
                    successors_by_block
                        .entry(block)
                        .or_insert_with(Vec::new)
                        .push(destination.block(&func.dfg.value_lists));
                    for value in destination.args_slice(&func.dfg.value_lists) {
                        push_free_value(func, *value, vmctx, &defined, &mut free);
                    }
                }
                InstructionData::Brif { blocks, .. } => {
                    for block_call in blocks {
                        successors_by_block
                            .entry(block)
                            .or_insert_with(Vec::new)
                            .push(block_call.block(&func.dfg.value_lists));
                        for value in block_call.args_slice(&func.dfg.value_lists) {
                            push_free_value(func, *value, vmctx, &defined, &mut free);
                        }
                    }
                }
                InstructionData::BranchTable { table, .. } => {
                    let table_data = &func.dfg.jump_tables[*table];
                    let successors = successors_by_block.entry(block).or_insert_with(Vec::new);
                    successors.push(table_data.default_block().block(&func.dfg.value_lists));
                    for block_call in table_data.as_slice() {
                        successors.push(block_call.block(&func.dfg.value_lists));
                    }
                }
                _ => {}
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

fn push_free_value(
    func: &ir::Function, value: ir::Value, vmctx: ir::Value, defined: &[ir::Value],
    free: &mut Vec<ir::Value>,
) {
    let value = func.dfg.resolve_aliases(value);
    if value != vmctx && !defined.contains(&value) && !free.contains(&value) {
        free.push(value);
    }
}

fn lower_block(
    module: &NixModule, func: &ir::Function, block: Block, vmctx: ir::Value,
    free_values: &BTreeMap<Block, Vec<ir::Value>>,
) -> Result<Expr> {
    // Block parameters: block params excluding vmctx, then `st`.
    let mut params: Vec<String> = func
        .dfg
        .block_params(block)
        .iter()
        .copied()
        .filter(|v| *v != vmctx)
        .map(value_name)
        .collect();
    params.extend(
        free_values
            .get(&block)
            .into_iter()
            .flat_map(|values| values.iter())
            .map(|v| value_name(*v)),
    );
    params.push("st".into());

    let body = lower_block_body(module, func, block, vmctx, free_values)?;
    Ok(lambda_chain(&params, body))
}

fn lower_block_body(
    module: &NixModule, func: &ir::Function, block: Block, vmctx: ir::Value,
    free_values: &BTreeMap<Block, Vec<ir::Value>>,
) -> Result<Expr> {
    let mut insts: Vec<Inst> = func.layout.block_insts(block).collect();
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
        let lowered = lower_inst(module, func, inst, &st_name, vmctx)?;
        if let Some(step) = lowered.step {
            steps.push(step);
        }
        if let Some(new_st) = lowered.next_state {
            st_name = new_st;
        }
    }

    let expr = lower_terminator(func, term, &st_name, vmctx, free_values)?;
    let lowered = inline_single_use_values(steps, expr);

    let mut expr = lowered.body;
    if !lowered.forces.is_empty() {
        let force_list = Expr::list(lowered.forces);
        let seq = || Expr::select(nix!(builtins), AttrKey::ident("seq"));
        let force_step = Expr::lambda(
            "acc",
            Expr::lambda(
                "force",
                Expr::app(
                    seq(),
                    vec![nix!(acc), Expr::app(seq(), vec![nix!(force), nix!(force)])],
                ),
            ),
        );
        let force_steps = Expr::app(
            Expr::select(nix!(builtins), AttrKey::string("foldl'")),
            vec![force_step, Expr::int(0), force_list],
        );
        expr = Expr::app(seq(), vec![force_steps, expr]);
    }

    Ok(Expr::let_binds(lowered.binds, expr))
}

struct LoweredBlockBody {
    binds: Vec<LetBind>,
    forces: Vec<Expr>,
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

        let force_uses = count_ident_uses_in_exprs(&forces, &name);
        let removable_force_count = forces
            .iter()
            .filter(|force| is_ident_named(force, &name))
            .count();
        if force_uses != removable_force_count || removable_force_count > 1 {
            index += 1;
            continue;
        }

        let use_count =
            count_ident_uses_in_binds(&binds[index + 1..], &name) + count_ident_uses(&body, &name);
        if use_count > 1 {
            index += 1;
            continue;
        }

        if use_count == 1 {
            for bind in &mut binds[index + 1..] {
                substitute_ident(&mut bind.value, &name, &value);
            }
            substitute_ident(&mut body, &name, &value);
        }
        if removable_force_count == 1 {
            remove_ident_force(&mut forces, &name);
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
        Expr::Int(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Null => true,
        Expr::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            is_inline_candidate(cond)
                && is_inline_candidate(then_expr)
                && is_inline_candidate(else_expr)
        }
        Expr::BinOp { op, lhs, rhs } => {
            is_safe_inline_binop(*op) && is_inline_candidate(lhs) && is_inline_candidate(rhs)
        }
        Expr::UnOp { expr, .. } => is_inline_candidate(expr),
        Expr::Ident(_)
        | Expr::Raw(_)
        | Expr::Path(_)
        | Expr::List(_)
        | Expr::AttrSet { .. }
        | Expr::Select { .. }
        | Expr::Lambda { .. }
        | Expr::LetIn { .. }
        | Expr::Apply { .. } => false,
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

fn remove_ident_force(forces: &mut Vec<Expr>, name: &str) {
    let mut removed = false;
    forces.retain(|force| {
        if !removed && is_ident_named(force, name) {
            removed = true;
            false
        } else {
            true
        }
    });
}

fn lower_terminator(
    func: &ir::Function, inst: Inst, st_name: &str, vmctx: ir::Value,
    free_values: &BTreeMap<Block, Vec<ir::Value>>,
) -> Result<Expr> {
    let data = &func.dfg.insts[inst];
    let op = data.opcode();

    Ok(match (op, data) {
        (Opcode::Return, InstructionData::MultiAry { args, .. }) => {
            let rvs = args
                .as_slice(&func.dfg.value_lists)
                .iter()
                .copied()
                .map(|v| value_expr(func, v, st_name, vmctx))
                .collect::<Vec<_>>();
            lower_done_record(Expr::ident(st_name), rvs)
        }

        (Opcode::Jump, InstructionData::Jump { destination, .. }) => {
            lower_block_call(func, *destination, st_name, vmctx, free_values)
        }

        (Opcode::Brif, InstructionData::Brif { blocks, arg, .. }) => {
            let cond = truthy_value_expr(func, *arg, st_name, vmctx);

            let then_call = lower_block_call(func, blocks[0], st_name, vmctx, free_values);
            let else_call = lower_block_call(func, blocks[1], st_name, vmctx, free_values);

            nix!(if #{cond} then #{then_call} else #{else_call})
        }

        (Opcode::BrTable, InstructionData::BranchTable { arg, table, .. }) => {
            lower_branch_table(func, inst, *arg, *table, st_name, vmctx, free_values)
        }

        (Opcode::Trap, _) => {
            let trap_code = data
                .trap_code()
                .map_or_else(|| "?".into(), |code| code.to_string());
            nix!(throw(#{format!("wasm2nix: trap {trap_code}")}))
        }

        (Opcode::Trapz, InstructionData::CondTrap { arg, code, .. }) => {
            let c = truthy_value_expr(func, *arg, st_name, vmctx);
            // trapz: if c is false then trap.
            nix!(if #{c}
                then #{lower_done_record(Expr::ident(st_name), vec![])}
                else throw(#{format!("wasm2nix: trapz {code}")}))
        }

        (Opcode::Trapnz, InstructionData::CondTrap { arg, code, .. }) => {
            let c = truthy_value_expr(func, *arg, st_name, vmctx);
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
    func: &ir::Function, inst: Inst, arg: ir::Value, table: ir::JumpTable, st_name: &str,
    vmctx: ir::Value, free_values: &BTreeMap<Block, Vec<ir::Value>>,
) -> Expr {
    let index_name = format!("br_table_index_{}", inst.as_u32());
    let targets_name = format!("br_table_targets_{}", inst.as_u32());
    let table_data = &func.dfg.jump_tables[table];
    let targets = table_data
        .as_slice()
        .iter()
        .copied()
        .map(|branch| lower_block_call(func, branch, st_name, vmctx, free_values))
        .collect::<Vec<_>>();
    let target_count = i64::try_from(targets.len()).expect("branch table target count fits in i64");
    let default = lower_block_call(
        func,
        table_data.default_block(),
        st_name,
        vmctx,
        free_values,
    );
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

    Expr::let_in(
        vec![
            (index_name, value_expr(func, arg, st_name, vmctx)),
            (targets_name, Expr::list(targets)),
        ],
        nix!(if #{in_range} then #{selected} else #{default}),
    )
}

fn lower_block_call(
    func: &ir::Function, call: ir::BlockCall, st_name: &str, vmctx: ir::Value,
    free_values: &BTreeMap<Block, Vec<ir::Value>>,
) -> Expr {
    let target = call.block(&func.dfg.value_lists);
    let mut args = call
        .args_slice(&func.dfg.value_lists)
        .iter()
        .copied()
        .map(|v| value_expr(func, v, st_name, vmctx))
        .collect::<Vec<_>>();
    args.extend(
        free_values
            .get(&target)
            .into_iter()
            .flat_map(|values| values.iter())
            .map(|v| value_expr(func, *v, st_name, vmctx)),
    );
    lower_jump_record(target, args, Expr::ident(st_name))
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
