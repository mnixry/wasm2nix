use cranelift_codegen::{
    ir,
    ir::{
        Inst, InstructionData, Opcode,
        condcodes::{FloatCC, IntCC},
        types::{I8, I16, I32, I64},
    },
};
use wasm2nix_runtime::{RuntimeFunction, RuntimeResultKind, intrinsics as intr};

use super::{
    error::{
        MissingTypeSnafu, Result, UnknownExternalNamespaceSnafu, UnknownIntrinsicSnafu,
        UnsupportedExternalNameSnafu, UnsupportedOpcodeSnafu,
    },
    lower_table_entry, ops, signature_id, types,
};
use crate::{
    nix::{BinOp, Expr, UnOp, nix},
    translator::NixModule,
};

pub(super) struct Step {
    pub(super) binds: Vec<(String, Expr)>,
    pub(super) force: Option<Force>,
}

/// Evaluation a block performs, in program order, before its terminator. Each
/// force lowers to an `assert` condition, which evaluates in place without a
/// call or a thunk for the rest of the block.
pub(super) enum Force {
    /// A bound SSA value. Values are numbers, booleans or small ref attrsets,
    /// so `v == v` is a cheap way to force one.
    Value(Expr),
    /// A bound state, which is too large to compare.
    State(Expr),
    /// Throws `message` when `cond` evaluates to `trap_on`, or unconditionally
    /// without a condition.
    Trap {
        cond: Option<Expr>,
        trap_on: bool,
        message: String,
    },
}

impl Force {
    pub(super) fn condition(self) -> Expr {
        match self {
            Self::Value(value) => nix!(#{value.clone()} == #{value}),
            Self::State(state) => nix!(builtins.seq(#{state}, true)),
            Self::Trap {
                cond: None,
                message,
                ..
            } => nix!(throw(#{message})),
            Self::Trap {
                cond: Some(cond),
                trap_on: true,
                message,
            } => Expr::bin(BinOp::Implies, cond, nix!(throw(#{message}))),
            Self::Trap {
                cond: Some(cond),
                trap_on: false,
                message,
            } => nix!(#{cond} || throw(#{message})),
        }
    }

    pub(super) const fn expr(&self) -> Option<&Expr> {
        match self {
            Self::Value(expr) | Self::State(expr) => Some(expr),
            Self::Trap { cond, .. } => cond.as_ref(),
        }
    }

    pub(super) const fn expr_mut(&mut self) -> Option<&mut Expr> {
        match self {
            Self::Value(expr) | Self::State(expr) => Some(expr),
            Self::Trap { cond, .. } => cond.as_mut(),
        }
    }
}

pub(super) struct LoweredInst {
    pub(super) step: Option<Step>,
    pub(super) next_state: Option<String>,
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

pub(super) fn lower_inst(
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
            InstructionData::Trap { code, .. } => Ok(Self::lower_trap(*code)),
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

        if let Some(e) = ops::inline_int_binary(
            self.op,
            out_ty,
            a.clone(),
            b.clone(),
            self.const_int(args[1]),
        ) {
            return Ok(self.pure_bind(out, e));
        }
        if self.op == Opcode::Iadd && out_ty == I32 && self.const_int(args[0]) == Some(0) {
            return Ok(self.pure_bind(out, b));
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

        let imm_const = match imm_expr {
            Expr::Int(value) => Some(value),
            _ => None,
        };
        if let Some(e) = ops::inline_int_binary(
            self.op,
            out_ty,
            self.value_expr(arg),
            imm_expr.clone(),
            imm_const,
        ) {
            return self.pure_bind(out, e);
        }
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
        let is_i64 = self.func.dfg.value_type(args[0]) == I64;
        let (lhs, rhs) = (self.value_expr(args[0]), self.value_expr(args[1]));
        if let Some(e) = ops::inline_int_compare(is_i64, cond, lhs.clone(), rhs.clone()) {
            return self.bind_result(e);
        }
        self.bind_result(Expr::app(
            int_compare_function(is_i64, cond).expr(),
            vec![lhs, rhs],
        ))
    }

    fn lower_int_compare_imm(&self, arg: ir::Value, cond: IntCC, imm64: i64) -> LoweredInst {
        let ty = self.func.dfg.value_type(arg);
        let imm_expr = if ty == I32 {
            Expr::int(i64::from(imm64.cast_unsigned() as u32))
        } else {
            Expr::int(imm64)
        };
        let lhs = self.value_expr(arg);
        if let Some(e) = ops::inline_int_compare(ty == I64, cond, lhs.clone(), imm_expr.clone()) {
            return self.bind_result(e);
        }
        self.bind_result(Expr::app(
            int_compare_function(ty == I64, cond).expr(),
            vec![lhs, imm_expr],
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
        LoweredInst::step(Step {
            binds: Vec::new(),
            force: Some(Force::Trap {
                cond: Some(self.truthy_expr(arg)),
                trap_on: self.op == Opcode::Trapnz,
                message: format!("wasm2nix: trap {code}"),
            }),
        })
    }

    fn lower_trap(code: ir::TrapCode) -> LoweredInst {
        LoweredInst::step(Step {
            binds: Vec::new(),
            force: Some(Force::Trap {
                cond: None,
                trap_on: true,
                message: format!("wasm2nix: trap {code}"),
            }),
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
                force: Some(Force::State(Expr::ident(new_st.clone()))),
            },
            new_st,
        )
    }

    fn stateful_state_step(&self, e: Expr) -> LoweredInst {
        let new_st = self.state_name();
        LoweredInst::step_and_state(
            Step {
                binds: vec![(new_st.clone(), e)],
                force: Some(Force::State(Expr::ident(new_st.clone()))),
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
            .or_else(|| binds.first().map(|(n, _)| Expr::ident(n.clone())))
            .map(Force::Value);
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

    /// The value of `v` when it is defined by a constant. Global values lower
    /// to 0, which makes the heap base addition foldable.
    fn const_int(&self, v: ir::Value) -> Option<i64> {
        let v = self.func.dfg.resolve_aliases(v);
        let inst = self.func.dfg.value_def(v).inst()?;
        match self.func.dfg.insts[inst] {
            InstructionData::UnaryImm {
                opcode: Opcode::Iconst,
                imm,
            } => Some(imm.bits()),
            InstructionData::UnaryGlobalValue {
                opcode: Opcode::GlobalValue | Opcode::SymbolValue | Opcode::TlsValue,
                ..
            } => Some(0),
            _ => None,
        }
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

const fn float_compare_function(ty: ir::Type, cc: FloatCC) -> Option<RuntimeFunction> {
    match (ty, cc) {
        (ir::types::F32, FloatCC::Ordered) => Some(RuntimeFunction::FcmpF32Ord),
        (ir::types::F32, FloatCC::Unordered) => Some(RuntimeFunction::FcmpF32Uno),
        (ir::types::F32, FloatCC::Equal) => Some(RuntimeFunction::FcmpF32Eq),
        (ir::types::F32, FloatCC::NotEqual) => Some(RuntimeFunction::FcmpF32Ne),
        (ir::types::F32, FloatCC::OrderedNotEqual) => Some(RuntimeFunction::FcmpF32One),
        (ir::types::F32, FloatCC::UnorderedOrEqual) => Some(RuntimeFunction::FcmpF32Ueq),
        (ir::types::F32, FloatCC::LessThan) => Some(RuntimeFunction::FcmpF32Lt),
        (ir::types::F32, FloatCC::LessThanOrEqual) => Some(RuntimeFunction::FcmpF32Le),
        (ir::types::F32, FloatCC::GreaterThan) => Some(RuntimeFunction::FcmpF32Gt),
        (ir::types::F32, FloatCC::GreaterThanOrEqual) => Some(RuntimeFunction::FcmpF32Ge),
        (ir::types::F32, FloatCC::UnorderedOrLessThan) => Some(RuntimeFunction::FcmpF32Ult),
        (ir::types::F32, FloatCC::UnorderedOrLessThanOrEqual) => Some(RuntimeFunction::FcmpF32Ule),
        (ir::types::F32, FloatCC::UnorderedOrGreaterThan) => Some(RuntimeFunction::FcmpF32Ugt),
        (ir::types::F32, FloatCC::UnorderedOrGreaterThanOrEqual) => {
            Some(RuntimeFunction::FcmpF32Uge)
        }
        (ir::types::F64, FloatCC::Ordered) => Some(RuntimeFunction::FcmpF64Ord),
        (ir::types::F64, FloatCC::Unordered) => Some(RuntimeFunction::FcmpF64Uno),
        (ir::types::F64, FloatCC::Equal) => Some(RuntimeFunction::FcmpF64Eq),
        (ir::types::F64, FloatCC::NotEqual) => Some(RuntimeFunction::FcmpF64Ne),
        (ir::types::F64, FloatCC::OrderedNotEqual) => Some(RuntimeFunction::FcmpF64One),
        (ir::types::F64, FloatCC::UnorderedOrEqual) => Some(RuntimeFunction::FcmpF64Ueq),
        (ir::types::F64, FloatCC::LessThan) => Some(RuntimeFunction::FcmpF64Lt),
        (ir::types::F64, FloatCC::LessThanOrEqual) => Some(RuntimeFunction::FcmpF64Le),
        (ir::types::F64, FloatCC::GreaterThan) => Some(RuntimeFunction::FcmpF64Gt),
        (ir::types::F64, FloatCC::GreaterThanOrEqual) => Some(RuntimeFunction::FcmpF64Ge),
        (ir::types::F64, FloatCC::UnorderedOrLessThan) => Some(RuntimeFunction::FcmpF64Ult),
        (ir::types::F64, FloatCC::UnorderedOrLessThanOrEqual) => Some(RuntimeFunction::FcmpF64Ule),
        (ir::types::F64, FloatCC::UnorderedOrGreaterThan) => Some(RuntimeFunction::FcmpF64Ugt),
        (ir::types::F64, FloatCC::UnorderedOrGreaterThanOrEqual) => {
            Some(RuntimeFunction::FcmpF64Uge)
        }
        _ => None,
    }
}

pub(super) fn value_name(v: ir::Value) -> String {
    format!("v{}", v.as_u32())
}

pub(super) fn value_expr(
    func: &ir::Function, v: ir::Value, st_name: &str, vmctx: ir::Value,
) -> Expr {
    let v = func.dfg.resolve_aliases(v);
    if v == vmctx {
        Expr::ident(st_name)
    } else {
        Expr::ident(value_name(v))
    }
}

pub(super) fn truthy_value_expr(
    func: &ir::Function, v: ir::Value, st_name: &str, vmctx: ir::Value,
) -> Expr {
    let value = value_expr(func, v, st_name, vmctx);
    if func.dfg.value_type(v) == I8 {
        value
    } else {
        nix!(#{value} != 0)
    }
}
