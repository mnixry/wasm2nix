//! Lowering of non-terminator instructions, by instruction family.

mod arith;
mod call;
mod compare;
mod convert;
mod memory;

use std::collections::BTreeSet;

pub(super) use arith::is_bit_op;
use cranelift_codegen::ir::{self, Inst, InstructionData, Opcode, types::I8};

use crate::{
    lower::error::{Result, UnsupportedOpcodeSnafu},
    nix::{BinOp, Expr, nix},
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
    offset_adds: &BTreeSet<Inst>,
) -> Result<LoweredInst> {
    InstCtx::new(module, func, inst, st_name, vmctx, offset_adds).lower()
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
    offset_adds: &'a BTreeSet<Inst>,
}

impl<'a> InstCtx<'a> {
    fn new(
        module: &'a NixModule, func: &'a ir::Function, inst: Inst, st_name: &'a str,
        vmctx: ir::Value, offset_adds: &'a BTreeSet<Inst>,
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
            offset_adds,
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
