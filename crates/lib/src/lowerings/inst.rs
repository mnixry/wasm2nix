use std::collections::BTreeMap;

use cranelift_codegen::{
    ir,
    ir::{
        ArgumentPurpose, Block, Inst, InstructionData, Opcode,
        condcodes::{FloatCC, IntCC},
        types::{I8, I16, I32, I64},
    },
};
use snafu::prelude::*;
use wasm2nix_runtime::{RuntimeFunction, RuntimeResultKind, intrinsics as intr};

use super::{
    error::{
        EmptyBlockSnafu, MissingEntryBlockSnafu, MissingTypeSnafu, MissingVmContextSnafu, Result,
        UnknownExternalNamespaceSnafu, UnknownIntrinsicSnafu, UnsupportedExternalNameSnafu,
        UnsupportedOpcodeSnafu,
    },
    lambda_chain, lower_table_entry, ops, signature_id, types,
};
use crate::{
    nix::{AttrBind, AttrKey, BinOp, Expr, LetBind, UnOp, nix},
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

struct Step {
    binds: Vec<(String, Expr)>,
    force: Option<Expr>,
}

struct LoweredInst {
    step: Option<Step>,
    next_state: Option<String>,
}

impl LoweredInst {
    const fn step(step: Step) -> Self {
        Self {
            step: Some(step),
            next_state: None,
        }
    }

    const fn step_and_state(step: Step, next_state: String) -> Self {
        Self {
            step: Some(step),
            next_state: Some(next_state),
        }
    }
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

fn lower_inst(
    module: &NixModule, func: &ir::Function, inst: Inst, st_name: &str, vmctx: ir::Value,
) -> Result<LoweredInst> {
    InstCtx::new(module, func, inst, st_name, vmctx).lower()
}

struct InstCtx<'a> {
    module: &'a NixModule,
    func: &'a ir::Function,
    inst: Inst,
    data: &'a InstructionData,
    op: Opcode,
    results: &'a [ir::Value],
    st_name: &'a str,
    vmctx: ir::Value,
}

impl<'a> InstCtx<'a> {
    fn new(
        module: &'a NixModule, func: &'a ir::Function, inst: Inst, st_name: &'a str,
        vmctx: ir::Value,
    ) -> Self {
        let data = &func.dfg.insts[inst];
        let op = data.opcode();
        let results = func.dfg.inst_results(inst);
        Self {
            module,
            func,
            inst,
            data,
            op,
            results,
            st_name,
            vmctx,
        }
    }

    fn lower(&self) -> Result<LoweredInst> {
        if self.op == Opcode::Bitcast {
            return Ok(self.lower_bitcast());
        }

        match self.data {
            InstructionData::UnaryImm { imm, .. } if self.op == Opcode::Iconst => {
                Ok(self.lower_iconst(i64::from(*imm)))
            }
            InstructionData::UnaryIeee32 { imm, .. } if self.op == Opcode::F32const => {
                Ok(self.bind_result(Expr::int(i64::from(imm.bits()))))
            }
            InstructionData::UnaryIeee64 { imm, .. } if self.op == Opcode::F64const => {
                Ok(self.bind_result(Expr::int(imm.bits().cast_signed())))
            }
            InstructionData::UnaryGlobalValue { .. }
                if matches!(
                    self.op,
                    Opcode::GlobalValue | Opcode::SymbolValue | Opcode::TlsValue
                ) =>
            {
                Ok(self.bind_result(Expr::int(0)))
            }
            InstructionData::Unary { arg, .. } => self.lower_unary(*arg),
            InstructionData::Binary { args, .. } => self.lower_binary(*args),
            InstructionData::BinaryImm64 { arg, imm, .. } => {
                Ok(self.lower_binary_imm(*arg, i64::from(*imm)))
            }
            InstructionData::Ternary { args, .. }
                if matches!(self.op, Opcode::Select | Opcode::SelectSpectreGuard) =>
            {
                Ok(self.lower_select(*args))
            }
            InstructionData::IntCompare { args, cond, .. } if self.op == Opcode::Icmp => {
                Ok(self.lower_int_compare(*args, *cond))
            }
            InstructionData::IntCompareImm { arg, cond, imm, .. } if self.op == Opcode::IcmpImm => {
                Ok(self.lower_int_compare_imm(*arg, *cond, i64::from(*imm)))
            }
            InstructionData::FloatCompare { args, cond, .. } if self.op == Opcode::Fcmp => {
                self.lower_float_compare(*args, *cond)
            }
            InstructionData::Load { arg, offset, .. } => {
                let off: i32 = (*offset).into();
                self.lower_load(*arg, off)
            }
            InstructionData::Store { args, offset, .. } => {
                let off: i32 = (*offset).into();
                self.lower_store(*args, off)
            }
            InstructionData::CondTrap { arg, code, .. }
                if matches!(self.op, Opcode::Trapz | Opcode::Trapnz) =>
            {
                Ok(self.lower_cond_trap(*arg, *code))
            }
            InstructionData::Trap { code, .. } => Ok(self.lower_trap(*code)),
            _ if self.op == Opcode::Call => self.lower_call(),
            _ => Ok(self.unsupported(format!("wasm2nix: unimplemented inst opcode {}", self.op))),
        }
    }

    fn lower_bitcast(&self) -> LoweredInst {
        let arg = self.data.arguments(&self.func.dfg.value_lists)[0];
        self.bind_result(self.value_expr(arg))
    }

    fn lower_iconst(&self, v: i64) -> LoweredInst {
        let dst = self.results[0];
        let ty = self.func.dfg.value_type(dst);
        let nix_int = if ty == I32 {
            // represent i32 as u32 in i64 range
            i64::from(v.cast_unsigned() as u32)
        } else {
            v
        };
        self.pure_bind(dst, Expr::int(nix_int))
    }

    fn lower_unary(&self, arg: ir::Value) -> Result<LoweredInst> {
        match self.op {
            Opcode::Uextend => Ok(self.lower_uextend(arg)),
            Opcode::Sextend => Ok(self.lower_sextend(arg)),
            Opcode::Ireduce => Ok(self.bind_result(Expr::app(
                RuntimeFunction::ConvIreduce.expr(),
                vec![self.value_expr(arg)],
            ))),
            Opcode::FcvtToSint
            | Opcode::FcvtToUint
            | Opcode::FcvtToSintSat
            | Opcode::FcvtToUintSat => self.lower_float_to_int(arg),
            Opcode::FcvtFromSint | Opcode::FcvtFromUint => self.lower_int_to_float(arg),
            Opcode::Fpromote | Opcode::Fdemote => Ok(self.lower_float_resize(arg)),
            Opcode::Bnot => Ok(self.bind_result(Expr::app(
                RuntimeFunction::IntBnot.expr(),
                vec![self.value_expr(arg)],
            ))),
            Opcode::Ineg => Ok(self.bind_result(Expr::un(UnOp::Neg, self.value_expr(arg)))),
            Opcode::Clz | Opcode::Ctz | Opcode::Popcnt => self.lower_int_unary(arg),
            Opcode::Fneg
            | Opcode::Fabs
            | Opcode::Sqrt
            | Opcode::Ceil
            | Opcode::Floor
            | Opcode::Trunc
            | Opcode::Nearest => self.lower_float_unary(arg),
            _ => Ok(self.unsupported(format!("wasm2nix: unimplemented unary op {}", self.op))),
        }
    }

    fn lower_uextend(&self, arg: ir::Value) -> LoweredInst {
        let out = self.results[0];
        let in_ty = self.func.dfg.value_type(arg);
        let out_ty = self.func.dfg.value_type(out);
        let a = self.value_expr(arg);

        let e = if in_ty == I8 && out_ty == I32 {
            nix!(if #{a} then 1 else 0)
        } else {
            // For now, rely on runtime for non-bool extensions.
            Expr::app(
                RuntimeFunction::ConvUextend.expr(),
                vec![a, Expr::string(out_ty.to_string())],
            )
        };
        self.pure_bind(out, e)
    }

    fn lower_sextend(&self, arg: ir::Value) -> LoweredInst {
        let out = self.results[0];
        let in_ty = self.func.dfg.value_type(arg);
        let out_ty = self.func.dfg.value_type(out);
        let a = self.value_expr(arg);
        let e = match (in_ty, out_ty) {
            (I8, I32) => RuntimeFunction::ConvSignExtend8.call(vec![a]),
            (I16, I32) => RuntimeFunction::ConvSignExtend16.call(vec![a]),
            (I8, I64) => RuntimeFunction::ConvSignExtend8I64.call(vec![a]),
            (I16, I64) => RuntimeFunction::ConvSignExtend16I64.call(vec![a]),
            (I32, I64) => RuntimeFunction::ConvSignExtend32.call(vec![a]),
            _ => RuntimeFunction::ConvSextend.call(vec![a]),
        };
        self.pure_bind(out, e)
    }

    fn lower_float_to_int(&self, arg: ir::Value) -> Result<LoweredInst> {
        let out = self.results[0];
        let in_ty = self.func.dfg.value_type(arg);
        let out_ty = self.func.dfg.value_type(out);
        let Some(function) = ops::float_to_int_conv_function(self.op, in_ty, out_ty) else {
            return self.unsupported_opcode();
        };
        let e = function.call(vec![self.value_expr(arg)]);
        Ok(self.pure_bind(out, e))
    }

    fn lower_int_to_float(&self, arg: ir::Value) -> Result<LoweredInst> {
        let out = self.results[0];
        let in_ty = self.func.dfg.value_type(arg);
        let out_ty = self.func.dfg.value_type(out);
        let Some(function) = ops::int_to_float_conv_function(self.op, in_ty, out_ty) else {
            return self.unsupported_opcode();
        };
        let e = function.call(vec![self.value_expr(arg)]);
        Ok(self.pure_bind(out, e))
    }

    fn lower_float_resize(&self, arg: ir::Value) -> LoweredInst {
        let function = if self.op == Opcode::Fpromote {
            RuntimeFunction::F64PromoteF32
        } else {
            RuntimeFunction::F32DemoteF64
        };
        self.bind_result(function.call(vec![self.value_expr(arg)]))
    }

    fn lower_int_unary(&self, arg: ir::Value) -> Result<LoweredInst> {
        let out = self.results[0];
        let out_ty = self.func.dfg.value_type(out);
        let Some(function) = ops::int_unary_function(self.op, out_ty) else {
            return self.unsupported_opcode();
        };
        Ok(self.pure_bind(out, function.call(vec![self.value_expr(arg)])))
    }

    fn lower_float_unary(&self, arg: ir::Value) -> Result<LoweredInst> {
        let out = self.results[0];
        let out_ty = self.func.dfg.value_type(out);
        let Some(function) = ops::float_unary_function(self.op, out_ty) else {
            return self.unsupported_opcode();
        };
        Ok(self.pure_bind(out, function.call(vec![self.value_expr(arg)])))
    }

    fn lower_binary(&self, args: [ir::Value; 2]) -> Result<LoweredInst> {
        let a = self.value_expr(args[0]);
        let b = self.value_expr(args[1]);
        let out = self.results[0];
        let out_ty = self.func.dfg.value_type(out);
        if out_ty == I8 {
            let e = match self.op {
                Opcode::Band => nix!(#{a} && #{b}),
                Opcode::Bor => nix!(#{a} || #{b}),
                Opcode::Bxor => nix!(#{a} != #{b}),
                _ => return self.unsupported_opcode(),
            };
            return Ok(self.pure_bind(out, e));
        }

        let Some(function) = ops::binary_function(self.op, out_ty) else {
            return self.unsupported_opcode();
        };
        Ok(self.pure_bind(out, function.call(vec![a, b])))
    }

    fn lower_binary_imm(&self, arg: ir::Value, imm64: i64) -> LoweredInst {
        let out = self.results[0];
        let out_ty = self.func.dfg.value_type(out);
        let imm_expr = if out_ty == I32 {
            Expr::int(i64::from(imm64.cast_unsigned() as u32))
        } else {
            Expr::int(imm64)
        };

        let Some(function) = ops::binary_imm_function(self.op, out_ty) else {
            return self.unsupported(format!("wasm2nix: unimplemented binary-imm op {}", self.op));
        };
        self.pure_bind(out, function.call(vec![self.value_expr(arg), imm_expr]))
    }

    fn lower_select(&self, args: [ir::Value; 3]) -> LoweredInst {
        let cond = self.truthy_expr(args[0]);
        let then_value = self.value_expr(args[1]);
        let else_value = self.value_expr(args[2]);
        self.bind_result(nix!(if #{cond} then #{then_value} else #{else_value}))
    }

    fn lower_int_compare(&self, args: [ir::Value; 2], cond: IntCC) -> LoweredInst {
        let function = int_compare_function(self.func.dfg.value_type(args[0]) == I64, cond);
        self.bind_result(Expr::app(
            function.expr(),
            vec![self.value_expr(args[0]), self.value_expr(args[1])],
        ))
    }

    fn lower_int_compare_imm(&self, arg: ir::Value, cond: IntCC, imm64: i64) -> LoweredInst {
        let ty = self.func.dfg.value_type(arg);
        let imm_expr = if ty == I32 {
            Expr::int(i64::from(imm64.cast_unsigned() as u32))
        } else {
            Expr::int(imm64)
        };
        let function = int_compare_function(ty == I64, cond);
        self.bind_result(Expr::app(
            function.expr(),
            vec![self.value_expr(arg), imm_expr],
        ))
    }

    fn lower_float_compare(&self, args: [ir::Value; 2], cond: FloatCC) -> Result<LoweredInst> {
        let ty = self.func.dfg.value_type(args[0]);
        let Some(function) = float_compare_function(ty, cond) else {
            return self.unsupported_opcode();
        };
        Ok(self.bind_result(Expr::app(
            function.expr(),
            vec![self.value_expr(args[0]), self.value_expr(args[1])],
        )))
    }

    fn lower_load(&self, arg: ir::Value, offset: i32) -> Result<LoweredInst> {
        let out = self.results[0];
        let out_ty = self.func.dfg.value_type(out);
        let addr = self.addr_with_offset(arg, offset);
        let e = Expr::app(
            types::load_function(self.op, out_ty)?.expr(),
            vec![addr, Expr::ident(self.st_name)],
        );
        Ok(self.pure_bind(out, e))
    }

    fn lower_store(&self, args: [ir::Value; 2], offset: i32) -> Result<LoweredInst> {
        let val = self.value_expr(args[0]);
        let addr = self.addr_with_offset(args[1], offset);
        let e = Expr::app(
            types::store_function(self.op, self.func.dfg.value_type(args[0]))?.expr(),
            vec![val, addr, Expr::ident(self.st_name)],
        );
        Ok(self.stateful_state_step(e))
    }

    fn lower_cond_trap(&self, arg: ir::Value, code: ir::TrapCode) -> LoweredInst {
        let trap_name = format!("trap_{}", self.inst.as_u32());
        let c = self.truthy_expr(arg);
        let trap_expr = nix!(throw(#{format!("wasm2nix: trap {code}")}));
        let trap = if self.op == Opcode::Trapz {
            nix!(if #{c} then 0 else #{trap_expr})
        } else {
            nix!(if #{c} then #{trap_expr} else 0)
        };
        LoweredInst::step(Step {
            binds: vec![(trap_name.clone(), trap)],
            force: Some(Expr::ident(trap_name)),
        })
    }

    fn lower_trap(&self, code: ir::TrapCode) -> LoweredInst {
        let trap_name = format!("trap_{}", self.inst.as_u32());
        LoweredInst::step(Step {
            binds: vec![(
                trap_name.clone(),
                nix!(throw(#{format!("wasm2nix: trap {code}")})),
            )],
            force: Some(Expr::ident(trap_name)),
        })
    }

    fn lower_call(&self) -> Result<LoweredInst> {
        let call_info = self.data.analyze_call(&self.func.dfg.value_lists);
        let ir::instructions::CallInfo::Direct(fref, args) = call_info else {
            return Ok(self.unsupported("wasm2nix: unsupported call kind"));
        };

        let callee = self.func.dfg.ext_funcs[fref].name.clone();
        let (callee_expr, call_kind) = resolve_callee(self.module, self.func, callee)?;

        match call_kind {
            CallKind::Pure => Ok(self.lower_pure_call(callee_expr, args)),
            CallKind::Indirect {
                table_index,
                type_id,
            } => Ok(self.lower_indirect_call(callee_expr, table_index, type_id, args)),
            CallKind::StatefulRecord => Ok(self.lower_stateful_record_call(callee_expr, args)),
            CallKind::StatefulState => Ok(self.lower_stateful_state_call(callee_expr, args)),
        }
    }

    fn lower_pure_call(&self, callee_expr: Expr, args: &[ir::Value]) -> LoweredInst {
        let e = Expr::app(callee_expr, self.arg_exprs(args));
        self.bind_result(e)
    }

    fn lower_indirect_call(
        &self, callee_expr: Expr, table_index: u32, type_id: String, args: &[ir::Value],
    ) -> LoweredInst {
        let callee_value = args
            .first()
            .copied()
            .map_or_else(|| Expr::int(0), |value| self.value_expr(value));
        let wasm_args = args
            .iter()
            .copied()
            .skip(1)
            .filter(|v| *v != self.vmctx)
            .map(|v| self.value_expr(v))
            .collect::<Vec<_>>();
        let indirect_call = Expr::app(
            callee_expr,
            vec![
                Expr::int(i64::from(table_index)),
                Expr::string(type_id),
                Expr::ident("moduleId"),
                Expr::ident("funcs"),
                Expr::ident("funcRegistry"),
                callee_value,
                Expr::list(wasm_args),
                Expr::ident(self.st_name),
            ],
        );
        self.record_call_step(indirect_call)
    }

    fn lower_stateful_record_call(&self, callee_expr: Expr, args: &[ir::Value]) -> LoweredInst {
        self.record_call_step(Expr::app(callee_expr, self.arg_exprs(args)))
    }

    fn lower_stateful_state_call(&self, callee_expr: Expr, args: &[ir::Value]) -> LoweredInst {
        self.stateful_state_step(Expr::app(callee_expr, self.arg_exprs(args)))
    }

    fn record_call_step(&self, call: Expr) -> LoweredInst {
        let tmp = format!("call_{}", self.inst.as_u32());
        let new_st = self.state_name();
        let mut binds = vec![
            (tmp.clone(), call),
            (new_st.clone(), nix!(#{Expr::ident(tmp.as_str())}.st)),
        ];
        self.push_record_results(&tmp, &mut binds);

        LoweredInst::step_and_state(
            Step {
                binds,
                force: Some(Expr::ident(new_st.clone())),
            },
            new_st,
        )
    }

    fn stateful_state_step(&self, e: Expr) -> LoweredInst {
        let new_st = self.state_name();
        LoweredInst::step_and_state(
            Step {
                binds: vec![(new_st.clone(), e)],
                force: Some(Expr::ident(new_st.clone())),
            },
            new_st,
        )
    }

    fn bind_result(&self, e: Expr) -> LoweredInst {
        self.pure_bind(self.results[0], e)
    }

    fn pure_bind(&self, out: ir::Value, e: Expr) -> LoweredInst {
        LoweredInst::step(self.seq_step(vec![(value_name(out), e)]))
    }

    fn seq_step(&self, binds: Vec<(String, Expr)>) -> Step {
        let force = self
            .results
            .first()
            .map(|v| Expr::ident(value_name(*v)))
            .or_else(|| binds.first().map(|(n, _)| Expr::ident(n.clone())));
        Step { binds, force }
    }

    fn unsupported(&self, message: impl Into<String>) -> LoweredInst {
        LoweredInst::step(Step {
            binds: self.unsupported_binds(message),
            force: None,
        })
    }

    fn unsupported_binds(&self, message: impl Into<String>) -> Vec<(String, Expr)> {
        let message = message.into();
        let throw_expr = || nix!(throw(#{message.clone()}));
        if self.results.is_empty() {
            vec![(format!("unsupported_{}", self.inst.as_u32()), throw_expr())]
        } else {
            self.results
                .iter()
                .copied()
                .map(|value| (value_name(value), throw_expr()))
                .collect()
        }
    }

    fn unsupported_opcode<T>(&self) -> Result<T> {
        UnsupportedOpcodeSnafu {
            opcode: self.op.to_string(),
        }
        .fail()
    }

    fn value_expr(&self, v: ir::Value) -> Expr {
        value_expr(self.func, v, self.st_name, self.vmctx)
    }

    fn truthy_expr(&self, v: ir::Value) -> Expr {
        truthy_value_expr(self.func, v, self.st_name, self.vmctx)
    }

    fn arg_exprs(&self, args: &[ir::Value]) -> Vec<Expr> {
        args.iter().copied().map(|v| self.value_expr(v)).collect()
    }

    fn addr_with_offset(&self, base_value: ir::Value, offset: i32) -> Expr {
        let base = self.value_expr(base_value);
        if offset == 0 {
            base
        } else {
            nix!(#{base} + #{i64::from(offset)})
        }
    }

    fn state_name(&self) -> String {
        format!("st_{}", self.inst.as_u32())
    }

    fn push_record_results(&self, tmp: &str, binds: &mut Vec<(String, Expr)>) {
        let results_list = nix!(#{Expr::ident(tmp)}.results);
        for (i, &rv) in self.results.iter().enumerate() {
            let idx = Expr::int(i64::try_from(i).expect("result index fits in i64"));
            let v = nix!(builtins.elemAt(#{results_list.clone()}, #{idx}));
            binds.push((value_name(rv), v));
        }
    }
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

#[derive(Debug, Clone)]
enum CallKind {
    Pure,
    StatefulRecord,
    StatefulState,
    Indirect { table_index: u32, type_id: String },
}

fn resolve_callee(
    module: &NixModule, func: &ir::Function, name: ir::ExternalName,
) -> Result<(Expr, CallKind)> {
    match name {
        ir::ExternalName::User(user_ref) => {
            let user = &func.params.user_named_funcs()[user_ref];
            match intr::UserNamespace::try_from(user.namespace) {
                Ok(intr::UserNamespace::Func) => {
                    // wasm function: f{index}
                    Ok((
                        Expr::ident(format!("f{}", user.index)),
                        CallKind::StatefulRecord,
                    ))
                }
                Ok(intr::UserNamespace::Intrinsic) => {
                    let intrinsic = intr::Intrinsic::try_from(user.index)
                        .map_err(|_| UnknownIntrinsicSnafu { index: user.index }.build())?;
                    resolve_intrinsic_callee(module, intrinsic, user.index)
                }
                Ok(intr::UserNamespace::Symbol) | Err(_) => UnknownExternalNamespaceSnafu {
                    namespace: user.namespace,
                }
                .fail(),
            }
        }
        other => UnsupportedExternalNameSnafu {
            name: format!("{other:?}"),
        }
        .fail(),
    }
}

fn resolve_intrinsic_callee(
    module: &NixModule, intrinsic: intr::Intrinsic, encoded_index: u32,
) -> Result<(Expr, CallKind)> {
    if let Some(function) = intrinsic.runtime_function() {
        let kind = intrinsic
            .runtime_result_kind()
            .expect("fixed runtime intrinsic has result kind");
        return Ok((function.expr(), runtime_result_to_call_kind(kind)));
    }

    match intrinsic {
        intr::Intrinsic::RefFunc { func_index } => {
            Ok((lower_table_entry(module, func_index), CallKind::Pure))
        }
        intr::Intrinsic::CallIndirect {
            table_index,
            type_index,
        } => {
            let sig_index = cranelift_wasm::TypeIndex::from_u32(type_index);
            let Some(sig) = module.info.signatures.get(sig_index) else {
                return MissingTypeSnafu { type_index }.fail();
            };
            Ok((
                RuntimeFunction::TableCall.expr(),
                CallKind::Indirect {
                    table_index,
                    type_id: signature_id(sig),
                },
            ))
        }
        _ => UnknownIntrinsicSnafu {
            index: encoded_index,
        }
        .fail(),
    }
}

const fn runtime_result_to_call_kind(kind: RuntimeResultKind) -> CallKind {
    match kind {
        RuntimeResultKind::Value => CallKind::Pure,
        RuntimeResultKind::State => CallKind::StatefulState,
        RuntimeResultKind::Record => CallKind::StatefulRecord,
    }
}

const fn int_compare_function(is_i64: bool, cc: IntCC) -> RuntimeFunction {
    match (is_i64, cc) {
        (false, IntCC::Equal) => RuntimeFunction::CmpEq,
        (false, IntCC::NotEqual) => RuntimeFunction::CmpNe,
        (false, IntCC::SignedLessThan) => RuntimeFunction::CmpLtS,
        (false, IntCC::SignedGreaterThanOrEqual) => RuntimeFunction::CmpGeS,
        (false, IntCC::SignedGreaterThan) => RuntimeFunction::CmpGtS,
        (false, IntCC::SignedLessThanOrEqual) => RuntimeFunction::CmpLeS,
        (false, IntCC::UnsignedLessThan) => RuntimeFunction::CmpLtU,
        (false, IntCC::UnsignedGreaterThanOrEqual) => RuntimeFunction::CmpGeU,
        (false, IntCC::UnsignedGreaterThan) => RuntimeFunction::CmpGtU,
        (false, IntCC::UnsignedLessThanOrEqual) => RuntimeFunction::CmpLeU,
        (true, IntCC::Equal) => RuntimeFunction::CmpI64Eq,
        (true, IntCC::NotEqual) => RuntimeFunction::CmpI64Ne,
        (true, IntCC::SignedLessThan) => RuntimeFunction::CmpI64LtS,
        (true, IntCC::SignedGreaterThanOrEqual) => RuntimeFunction::CmpI64GeS,
        (true, IntCC::SignedGreaterThan) => RuntimeFunction::CmpI64GtS,
        (true, IntCC::SignedLessThanOrEqual) => RuntimeFunction::CmpI64LeS,
        (true, IntCC::UnsignedLessThan) => RuntimeFunction::CmpI64LtU,
        (true, IntCC::UnsignedGreaterThanOrEqual) => RuntimeFunction::CmpI64GeU,
        (true, IntCC::UnsignedGreaterThan) => RuntimeFunction::CmpI64GtU,
        (true, IntCC::UnsignedLessThanOrEqual) => RuntimeFunction::CmpI64LeU,
    }
}

const fn float_compare_function(
    ty: cranelift_codegen::ir::Type, cc: FloatCC,
) -> Option<RuntimeFunction> {
    match (ty, cc) {
        (cranelift_codegen::ir::types::F32, FloatCC::Ordered) => Some(RuntimeFunction::FcmpF32Ord),
        (cranelift_codegen::ir::types::F32, FloatCC::Unordered) => {
            Some(RuntimeFunction::FcmpF32Uno)
        }
        (cranelift_codegen::ir::types::F32, FloatCC::Equal) => Some(RuntimeFunction::FcmpF32Eq),
        (cranelift_codegen::ir::types::F32, FloatCC::NotEqual) => Some(RuntimeFunction::FcmpF32Ne),
        (cranelift_codegen::ir::types::F32, FloatCC::OrderedNotEqual) => {
            Some(RuntimeFunction::FcmpF32One)
        }
        (cranelift_codegen::ir::types::F32, FloatCC::UnorderedOrEqual) => {
            Some(RuntimeFunction::FcmpF32Ueq)
        }
        (cranelift_codegen::ir::types::F32, FloatCC::LessThan) => Some(RuntimeFunction::FcmpF32Lt),
        (cranelift_codegen::ir::types::F32, FloatCC::LessThanOrEqual) => {
            Some(RuntimeFunction::FcmpF32Le)
        }
        (cranelift_codegen::ir::types::F32, FloatCC::GreaterThan) => {
            Some(RuntimeFunction::FcmpF32Gt)
        }
        (cranelift_codegen::ir::types::F32, FloatCC::GreaterThanOrEqual) => {
            Some(RuntimeFunction::FcmpF32Ge)
        }
        (cranelift_codegen::ir::types::F32, FloatCC::UnorderedOrLessThan) => {
            Some(RuntimeFunction::FcmpF32Ult)
        }
        (cranelift_codegen::ir::types::F32, FloatCC::UnorderedOrLessThanOrEqual) => {
            Some(RuntimeFunction::FcmpF32Ule)
        }
        (cranelift_codegen::ir::types::F32, FloatCC::UnorderedOrGreaterThan) => {
            Some(RuntimeFunction::FcmpF32Ugt)
        }
        (cranelift_codegen::ir::types::F32, FloatCC::UnorderedOrGreaterThanOrEqual) => {
            Some(RuntimeFunction::FcmpF32Uge)
        }
        (cranelift_codegen::ir::types::F64, FloatCC::Ordered) => Some(RuntimeFunction::FcmpF64Ord),
        (cranelift_codegen::ir::types::F64, FloatCC::Unordered) => {
            Some(RuntimeFunction::FcmpF64Uno)
        }
        (cranelift_codegen::ir::types::F64, FloatCC::Equal) => Some(RuntimeFunction::FcmpF64Eq),
        (cranelift_codegen::ir::types::F64, FloatCC::NotEqual) => Some(RuntimeFunction::FcmpF64Ne),
        (cranelift_codegen::ir::types::F64, FloatCC::OrderedNotEqual) => {
            Some(RuntimeFunction::FcmpF64One)
        }
        (cranelift_codegen::ir::types::F64, FloatCC::UnorderedOrEqual) => {
            Some(RuntimeFunction::FcmpF64Ueq)
        }
        (cranelift_codegen::ir::types::F64, FloatCC::LessThan) => Some(RuntimeFunction::FcmpF64Lt),
        (cranelift_codegen::ir::types::F64, FloatCC::LessThanOrEqual) => {
            Some(RuntimeFunction::FcmpF64Le)
        }
        (cranelift_codegen::ir::types::F64, FloatCC::GreaterThan) => {
            Some(RuntimeFunction::FcmpF64Gt)
        }
        (cranelift_codegen::ir::types::F64, FloatCC::GreaterThanOrEqual) => {
            Some(RuntimeFunction::FcmpF64Ge)
        }
        (cranelift_codegen::ir::types::F64, FloatCC::UnorderedOrLessThan) => {
            Some(RuntimeFunction::FcmpF64Ult)
        }
        (cranelift_codegen::ir::types::F64, FloatCC::UnorderedOrLessThanOrEqual) => {
            Some(RuntimeFunction::FcmpF64Ule)
        }
        (cranelift_codegen::ir::types::F64, FloatCC::UnorderedOrGreaterThan) => {
            Some(RuntimeFunction::FcmpF64Ugt)
        }
        (cranelift_codegen::ir::types::F64, FloatCC::UnorderedOrGreaterThanOrEqual) => {
            Some(RuntimeFunction::FcmpF64Uge)
        }
        _ => None,
    }
}

fn value_name(v: ir::Value) -> String {
    format!("v{}", v.as_u32())
}

fn block_name(b: Block) -> String {
    format!("b{}", b.as_u32())
}

fn value_expr(func: &ir::Function, v: ir::Value, st_name: &str, vmctx: ir::Value) -> Expr {
    let v = func.dfg.resolve_aliases(v);
    if v == vmctx {
        Expr::ident(st_name)
    } else {
        Expr::ident(value_name(v))
    }
}

fn truthy_value_expr(func: &ir::Function, v: ir::Value, st_name: &str, vmctx: ir::Value) -> Expr {
    let value = value_expr(func, v, st_name, vmctx);
    if func.dfg.value_type(v) == I8 {
        value
    } else {
        nix!(#{value} != 0)
    }
}
