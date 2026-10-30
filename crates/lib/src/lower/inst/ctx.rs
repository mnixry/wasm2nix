use std::collections::BTreeSet;

use cranelift_codegen::ir::Inst;
use cranelift_codegen::ir::InstructionData;
use cranelift_codegen::ir::Opcode;
use cranelift_codegen::ir::types::I8;
use cranelift_codegen::ir::types::I32;
use cranelift_codegen::ir::{self};

use super::step::Force;
use super::step::LoweredInst;
use super::step::Step;
use crate::lower::error::Result;
use crate::lower::error::UnsupportedOpcodeSnafu;
use crate::lower::function::BlockLowering;
use crate::lower::names::value_name;
use crate::nix::Expr;
use crate::nix::LetBind;
use crate::nix::nix;
use crate::translator::NixModule;

/// One instruction being lowered, with the state variable it reads.
pub(in crate::lower) struct InstCtx<'a> {
    pub(super) module: &'a NixModule,
    pub(super) func: &'a ir::Function,
    pub(super) inst: Inst,
    pub(super) data: &'a InstructionData,
    pub(super) op: Opcode,
    pub(super) results: &'a [ir::Value],
    pub(super) st_name: &'a str,
    pub(super) vmctx: ir::Value,
    pub(super) offset_adds: &'a BTreeSet<Inst>,
}

impl<'a> InstCtx<'a> {
    pub(in crate::lower) fn new(
        lowering: &BlockLowering<'a>, inst: Inst, st_name: &'a str,
    ) -> Self {
        let func = lowering.func;
        let data = &func.dfg.insts[inst];
        Self {
            module: lowering.module,
            func,
            inst,
            data,
            op: data.opcode(),
            results: func.dfg.inst_results(inst),
            st_name,
            vmctx: lowering.vmctx,
            offset_adds: lowering.offset_adds,
        }
    }

    pub(in crate::lower) fn lower(&self) -> Result<LoweredInst> {
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
            _ if self.op == Opcode::Call => self.lower_call(),
            _ => Ok(self.unsupported(&format!("wasm2nix: unimplemented inst opcode {}", self.op))),
        }
    }

    /// `trapz` and `trapnz`; `trap` always ends a block, so it is a terminator.
    fn lower_cond_trap(&self, arg: ir::Value, code: ir::TrapCode) -> LoweredInst {
        let cond = self.truthy_expr(arg);
        let message = format!("wasm2nix: trap {code}");
        let force = if self.op == Opcode::Trapnz {
            Force::TrapIf { cond, message }
        } else {
            Force::TrapUnless { cond, message }
        };
        LoweredInst::step(Step {
            binds: Vec::new(),
            force: Some(force),
        })
    }

    pub(super) fn record_call_step(&self, call: Expr) -> LoweredInst {
        let tmp = format!("call_{}", self.inst.as_u32());
        let new_st = self.state_name();
        let mut binds = vec![
            LetBind::new(tmp.clone(), call),
            LetBind::new(new_st.clone(), nix!(#{Expr::ident(tmp.as_str())}.st)),
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

    pub(super) fn stateful_state_step(&self, e: Expr) -> LoweredInst {
        let new_st = self.state_name();
        LoweredInst::step_and_state(
            Step {
                binds: vec![LetBind::new(new_st.clone(), e)],
                force: Some(Force::State(Expr::ident(new_st.clone()))),
            },
            new_st,
        )
    }

    pub(super) fn bind_result(&self, e: Expr) -> LoweredInst {
        self.pure_bind(self.results[0], e)
    }

    pub(super) fn pure_bind(&self, out: ir::Value, e: Expr) -> LoweredInst {
        LoweredInst::step(self.seq_step(vec![LetBind::new(value_name(out), e)]))
    }

    pub(super) fn seq_step(&self, binds: Vec<LetBind>) -> Step {
        let force = self
            .results
            .first()
            .map(|v| Expr::ident(value_name(*v)))
            .or_else(|| binds.first().map(|bind| Expr::ident(bind.name.as_str())))
            .map(Force::Value);
        Step { binds, force }
    }

    /// Binds the instruction's results to a `throw`, so the failure only
    /// happens if they are used.
    pub(super) fn unsupported(&self, message: &str) -> LoweredInst {
        LoweredInst::step(Step {
            binds: self.unsupported_binds(message),
            force: None,
        })
    }

    fn unsupported_binds(&self, message: &str) -> Vec<LetBind> {
        let throw_expr = || nix!(throw(#{message}));
        if self.results.is_empty() {
            vec![LetBind::new(
                format!("unsupported_{}", self.inst.as_u32()),
                throw_expr(),
            )]
        } else {
            self.results
                .iter()
                .map(|value| LetBind::new(value_name(*value), throw_expr()))
                .collect()
        }
    }

    pub(super) fn unsupported_opcode<T>(&self) -> Result<T> {
        UnsupportedOpcodeSnafu { opcode: self.op }.fail()
    }

    pub(super) fn value_expr(&self, v: ir::Value) -> Expr {
        value_expr(self.func, v, self.st_name, self.vmctx)
    }

    /// The value of `v` when it is defined by a constant. Global values lower
    /// to 0, which makes the heap base addition foldable.
    pub(super) fn const_int(&self, v: ir::Value) -> Option<i64> {
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

    pub(super) fn truthy_expr(&self, v: ir::Value) -> Expr {
        truthy_value_expr(self.func, v, self.st_name, self.vmctx)
    }

    pub(super) fn arg_exprs(&self, args: &[ir::Value]) -> Vec<Expr> {
        args.iter().copied().map(|v| self.value_expr(v)).collect()
    }

    pub(super) fn addr_with_offset(&self, base_value: ir::Value, offset: i32) -> Expr {
        let base = self.value_expr(base_value);
        if offset == 0 {
            base
        } else {
            nix!(#{base} + #{i64::from(offset)})
        }
    }

    pub(super) fn state_name(&self) -> String {
        format!("st_{}", self.inst.as_u32())
    }
}

/// An immediate in the value representation: i32 values are canonical u32.
pub(super) fn canonical_imm(ty: ir::Type, imm: i64) -> i64 {
    if ty == I32 {
        i64::from(imm.cast_unsigned() as u32)
    } else {
        imm
    }
}

pub(in crate::lower) fn value_expr(
    func: &ir::Function, v: ir::Value, st_name: &str, vmctx: ir::Value,
) -> Expr {
    let v = func.dfg.resolve_aliases(v);
    if v == vmctx {
        Expr::ident(st_name)
    } else {
        Expr::ident(value_name(v))
    }
}

pub(in crate::lower) fn truthy_value_expr(
    func: &ir::Function, v: ir::Value, st_name: &str, vmctx: ir::Value,
) -> Expr {
    let value = value_expr(func, v, st_name, vmctx);
    if func.dfg.value_type(v) == I8 {
        value
    } else {
        nix!(#{value} != 0)
    }
}
