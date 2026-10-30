use cranelift_codegen::ir::condcodes::FloatCC;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types::I64;
use cranelift_codegen::ir::{self};
use wasm2nix_runtime::RuntimeFunction;

use super::ctx::InstCtx;
use super::ctx::canonical_imm;
use super::step::LoweredInst;
use crate::lower::error::Result;
use crate::nix::BinOp;
use crate::nix::Expr;

impl InstCtx<'_> {
    pub(super) fn lower_int_compare(&self, args: [ir::Value; 2], cond: IntCC) -> LoweredInst {
        let ty = self.func.dfg.value_type(args[0]);
        self.int_compare(ty, cond, self.value_expr(args[0]), self.value_expr(args[1]))
    }

    pub(super) fn lower_int_compare_imm(
        &self, arg: ir::Value, cond: IntCC, imm64: i64,
    ) -> LoweredInst {
        let ty = self.func.dfg.value_type(arg);
        let imm = Expr::int(canonical_imm(ty, imm64));
        self.int_compare(ty, cond, self.value_expr(arg), imm)
    }

    fn int_compare(&self, ty: ir::Type, cond: IntCC, lhs: Expr, rhs: Expr) -> LoweredInst {
        let e = match inline_int_compare(ty, cond) {
            Some(op) => Expr::bin(op, lhs, rhs),
            None => int_compare_function(ty, cond).call([lhs, rhs]),
        };
        self.bind_result(e)
    }

    pub(super) fn lower_float_compare(
        &self, args: [ir::Value; 2], cond: FloatCC,
    ) -> Result<LoweredInst> {
        let ty = self.func.dfg.value_type(args[0]);
        let Some(function) = float_compare_function(ty, cond) else {
            return self.unsupported_opcode();
        };
        Ok(self.bind_result(function.call([self.value_expr(args[0]), self.value_expr(args[1])])))
    }
}

fn int_compare_function(ty: ir::Type, cc: IntCC) -> RuntimeFunction {
    match (ty == I64, cc) {
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

/// Integer comparisons that map onto Nix's own operators: equality for both
/// widths, unsigned order on canonical u32 and signed order on native int64.
fn inline_int_compare(ty: ir::Type, cc: IntCC) -> Option<BinOp> {
    Some(match (ty == I64, cc) {
        (_, IntCC::Equal) => BinOp::Eq,
        (_, IntCC::NotEqual) => BinOp::Neq,
        (false, IntCC::UnsignedLessThan) | (true, IntCC::SignedLessThan) => BinOp::Lt,
        (false, IntCC::UnsignedLessThanOrEqual) | (true, IntCC::SignedLessThanOrEqual) => BinOp::Le,
        (false, IntCC::UnsignedGreaterThan) | (true, IntCC::SignedGreaterThan) => BinOp::Gt,
        (false, IntCC::UnsignedGreaterThanOrEqual) | (true, IntCC::SignedGreaterThanOrEqual) => {
            BinOp::Ge
        }
        _ => return None,
    })
}
