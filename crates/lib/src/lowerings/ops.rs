use cranelift_codegen::ir::{
    Opcode, Type,
    condcodes::IntCC,
    types::{F32, F64, I32, I64},
};
use wasm2nix_runtime::RuntimeFunction;

use crate::nix::{AttrKey, BinOp, Expr};

const U32_MASK: i64 = 0xFFFF_FFFF;

/// Integer operations cheap enough to emit as inline Nix instead of a runtime
/// call. i32 values are canonical u32 and i64 values native int64, so bitwise
/// operations apply directly and i32 wraparound is a mask. `rhs_const` is the
/// right operand when it is a known constant.
pub(super) fn inline_int_binary(
    op: Opcode, ty: Type, lhs: Expr, rhs: Expr, rhs_const: Option<i64>,
) -> Option<Expr> {
    let mask32 = |expr| bit_op("bitAnd", expr, Expr::int(U32_MASK));
    // Shifting a u32 left by at most 31 bits stays below 2^63.
    let shift_scale = |amount: i64| Expr::int(1 << (amount & 31));
    match (ty, op) {
        (I32, Opcode::Iadd | Opcode::IaddImm) if rhs_const == Some(0) => Some(lhs),
        (I32, Opcode::Iadd | Opcode::IaddImm) => Some(mask32(Expr::bin(BinOp::Add, lhs, rhs))),
        (I32, Opcode::Isub) => Some(mask32(Expr::bin(BinOp::Sub, lhs, rhs))),
        (I32 | I64, Opcode::Band | Opcode::BandImm) => Some(bit_op("bitAnd", lhs, rhs)),
        (I32 | I64, Opcode::Bor | Opcode::BorImm) => Some(bit_op("bitOr", lhs, rhs)),
        (I32 | I64, Opcode::Bxor | Opcode::BxorImm) => Some(bit_op("bitXor", lhs, rhs)),
        (I32, Opcode::Ishl | Opcode::IshlImm) => {
            rhs_const.map(|amount| mask32(Expr::bin(BinOp::Mul, lhs, shift_scale(amount))))
        }
        (I32, Opcode::Ushr | Opcode::UshrImm) => {
            rhs_const.map(|amount| Expr::bin(BinOp::Div, lhs, shift_scale(amount)))
        }
        _ => None,
    }
}

/// Integer comparisons that map onto Nix's own operators: equality for both
/// widths, unsigned order on canonical u32 and signed order on native int64.
pub(super) fn inline_int_compare(is_i64: bool, cc: IntCC, lhs: Expr, rhs: Expr) -> Option<Expr> {
    let op = match (is_i64, cc) {
        (_, IntCC::Equal) => BinOp::Eq,
        (_, IntCC::NotEqual) => BinOp::Neq,
        (false, IntCC::UnsignedLessThan) | (true, IntCC::SignedLessThan) => BinOp::Lt,
        (false, IntCC::UnsignedLessThanOrEqual) | (true, IntCC::SignedLessThanOrEqual) => BinOp::Le,
        (false, IntCC::UnsignedGreaterThan) | (true, IntCC::SignedGreaterThan) => BinOp::Gt,
        (false, IntCC::UnsignedGreaterThanOrEqual) | (true, IntCC::SignedGreaterThanOrEqual) => {
            BinOp::Ge
        }
        _ => return None,
    };
    Some(Expr::bin(op, lhs, rhs))
}

fn bit_op(name: &str, lhs: Expr, rhs: Expr) -> Expr {
    Expr::app(
        Expr::select(Expr::ident("builtins"), AttrKey::ident(name)),
        [lhs, rhs],
    )
}

/// Whether `expr` is one of the bitwise builtins emitted by
/// [`inline_int_binary`], which are pure on integers.
pub(super) fn is_bit_op(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Select { expr, path }
            if matches!(expr.as_ref(), Expr::Ident(name) if name == "builtins")
                && matches!(
                    path.iter().collect::<Vec<_>>().as_slice(),
                    [AttrKey::Ident(name)] if matches!(name.as_str(), "bitAnd" | "bitOr" | "bitXor")
                )
    )
}

pub(super) const fn float_to_int_conv_function(
    op: Opcode, input: cranelift_codegen::ir::Type, output: cranelift_codegen::ir::Type,
) -> Option<RuntimeFunction> {
    match (op, input, output) {
        (Opcode::FcvtToSint, F32, I32) => Some(RuntimeFunction::ConvTruncSF32I32),
        (Opcode::FcvtToSint, F32, I64) => Some(RuntimeFunction::ConvTruncSF32I64),
        (Opcode::FcvtToSint, F64, I32) => Some(RuntimeFunction::ConvTruncSF64I32),
        (Opcode::FcvtToSint, F64, I64) => Some(RuntimeFunction::ConvTruncSF64I64),
        (Opcode::FcvtToUint, F32, I32) => Some(RuntimeFunction::ConvTruncUF32I32),
        (Opcode::FcvtToUint, F32, I64) => Some(RuntimeFunction::ConvTruncUF32I64),
        (Opcode::FcvtToUint, F64, I32) => Some(RuntimeFunction::ConvTruncUF64I32),
        (Opcode::FcvtToUint, F64, I64) => Some(RuntimeFunction::ConvTruncUF64I64),
        (Opcode::FcvtToSintSat, F32, I32) => Some(RuntimeFunction::ConvTruncSatSF32I32),
        (Opcode::FcvtToSintSat, F32, I64) => Some(RuntimeFunction::ConvTruncSatSF32I64),
        (Opcode::FcvtToSintSat, F64, I32) => Some(RuntimeFunction::ConvTruncSatSF64I32),
        (Opcode::FcvtToSintSat, F64, I64) => Some(RuntimeFunction::ConvTruncSatSF64I64),
        (Opcode::FcvtToUintSat, F32, I32) => Some(RuntimeFunction::ConvTruncSatUF32I32),
        (Opcode::FcvtToUintSat, F32, I64) => Some(RuntimeFunction::ConvTruncSatUF32I64),
        (Opcode::FcvtToUintSat, F64, I32) => Some(RuntimeFunction::ConvTruncSatUF64I32),
        (Opcode::FcvtToUintSat, F64, I64) => Some(RuntimeFunction::ConvTruncSatUF64I64),
        _ => None,
    }
}

pub(super) const fn int_to_float_conv_function(
    op: Opcode, input: cranelift_codegen::ir::Type, output: cranelift_codegen::ir::Type,
) -> Option<RuntimeFunction> {
    match (op, input, output) {
        (Opcode::FcvtFromSint, I32, F32) => Some(RuntimeFunction::ConvConvertSI32F32),
        (Opcode::FcvtFromSint, I32, F64) => Some(RuntimeFunction::ConvConvertSI32F64),
        (Opcode::FcvtFromSint, I64, F32) => Some(RuntimeFunction::ConvConvertSI64F32),
        (Opcode::FcvtFromSint, I64, F64) => Some(RuntimeFunction::ConvConvertSI64F64),
        (Opcode::FcvtFromUint, I32, F32) => Some(RuntimeFunction::ConvConvertUI32F32),
        (Opcode::FcvtFromUint, I32, F64) => Some(RuntimeFunction::ConvConvertUI32F64),
        (Opcode::FcvtFromUint, I64, F32) => Some(RuntimeFunction::ConvConvertUI64F32),
        (Opcode::FcvtFromUint, I64, F64) => Some(RuntimeFunction::ConvConvertUI64F64),
        _ => None,
    }
}

pub(super) const fn int_unary_function(
    op: Opcode, ty: cranelift_codegen::ir::Type,
) -> Option<RuntimeFunction> {
    match (ty, op) {
        (I32, Opcode::Clz) => Some(RuntimeFunction::I32Clz),
        (I32, Opcode::Ctz) => Some(RuntimeFunction::I32Ctz),
        (I32, Opcode::Popcnt) => Some(RuntimeFunction::I32Popcnt),
        (I64, Opcode::Clz) => Some(RuntimeFunction::I64Clz),
        (I64, Opcode::Ctz) => Some(RuntimeFunction::I64Ctz),
        (I64, Opcode::Popcnt) => Some(RuntimeFunction::I64Popcnt),
        _ => None,
    }
}

pub(super) const fn float_unary_function(
    op: Opcode, ty: cranelift_codegen::ir::Type,
) -> Option<RuntimeFunction> {
    match (ty, op) {
        (F32, Opcode::Fneg) => Some(RuntimeFunction::F32Neg),
        (F32, Opcode::Fabs) => Some(RuntimeFunction::F32Abs),
        (F32, Opcode::Sqrt) => Some(RuntimeFunction::F32Sqrt),
        (F32, Opcode::Ceil) => Some(RuntimeFunction::F32Ceil),
        (F32, Opcode::Floor) => Some(RuntimeFunction::F32Floor),
        (F32, Opcode::Trunc) => Some(RuntimeFunction::F32Trunc),
        (F32, Opcode::Nearest) => Some(RuntimeFunction::F32Nearest),
        (F64, Opcode::Fneg) => Some(RuntimeFunction::F64Neg),
        (F64, Opcode::Fabs) => Some(RuntimeFunction::F64Abs),
        (F64, Opcode::Sqrt) => Some(RuntimeFunction::F64Sqrt),
        (F64, Opcode::Ceil) => Some(RuntimeFunction::F64Ceil),
        (F64, Opcode::Floor) => Some(RuntimeFunction::F64Floor),
        (F64, Opcode::Trunc) => Some(RuntimeFunction::F64Trunc),
        (F64, Opcode::Nearest) => Some(RuntimeFunction::F64Nearest),
        _ => None,
    }
}

pub(super) const fn binary_function(
    op: Opcode, ty: cranelift_codegen::ir::Type,
) -> Option<RuntimeFunction> {
    match (ty, op) {
        (I32, Opcode::Iadd) => Some(RuntimeFunction::I32Add),
        (I32, Opcode::Isub) => Some(RuntimeFunction::I32Sub),
        (I32, Opcode::Imul) => Some(RuntimeFunction::I32Mul),
        (I32, Opcode::Udiv) => Some(RuntimeFunction::I32Udiv),
        (I32, Opcode::Sdiv) => Some(RuntimeFunction::I32Sdiv),
        (I32, Opcode::Urem) => Some(RuntimeFunction::I32Urem),
        (I32, Opcode::Srem) => Some(RuntimeFunction::I32Srem),
        (I32, Opcode::Band) => Some(RuntimeFunction::I32Band),
        (I32, Opcode::Bor) => Some(RuntimeFunction::I32Bor),
        (I32, Opcode::Bxor) => Some(RuntimeFunction::I32Bxor),
        (I32, Opcode::Ishl) => Some(RuntimeFunction::I32Shl),
        (I32, Opcode::Ushr) => Some(RuntimeFunction::I32ShrU),
        (I32, Opcode::Sshr) => Some(RuntimeFunction::I32ShrS),
        (I32, Opcode::Rotl) => Some(RuntimeFunction::I32Rotl),
        (I32, Opcode::Rotr) => Some(RuntimeFunction::I32Rotr),
        (I64, Opcode::Iadd) => Some(RuntimeFunction::I64Add),
        (I64, Opcode::Isub) => Some(RuntimeFunction::I64Sub),
        (I64, Opcode::Imul) => Some(RuntimeFunction::I64Mul),
        (I64, Opcode::Udiv) => Some(RuntimeFunction::I64Udiv),
        (I64, Opcode::Sdiv) => Some(RuntimeFunction::I64Sdiv),
        (I64, Opcode::Urem) => Some(RuntimeFunction::I64Urem),
        (I64, Opcode::Srem) => Some(RuntimeFunction::I64Srem),
        (I64, Opcode::Band) => Some(RuntimeFunction::I64Band),
        (I64, Opcode::Bor) => Some(RuntimeFunction::I64Bor),
        (I64, Opcode::Bxor) => Some(RuntimeFunction::I64Bxor),
        (I64, Opcode::Ishl) => Some(RuntimeFunction::I64Shl),
        (I64, Opcode::Ushr) => Some(RuntimeFunction::I64ShrU),
        (I64, Opcode::Sshr) => Some(RuntimeFunction::I64ShrS),
        (I64, Opcode::Rotl) => Some(RuntimeFunction::I64Rotl),
        (I64, Opcode::Rotr) => Some(RuntimeFunction::I64Rotr),
        (F32, Opcode::Fadd) => Some(RuntimeFunction::F32Add),
        (F32, Opcode::Fsub) => Some(RuntimeFunction::F32Sub),
        (F32, Opcode::Fmul) => Some(RuntimeFunction::F32Mul),
        (F32, Opcode::Fdiv) => Some(RuntimeFunction::F32Div),
        (F32, Opcode::Fmin) => Some(RuntimeFunction::F32Min),
        (F32, Opcode::Fmax) => Some(RuntimeFunction::F32Max),
        (F32, Opcode::Fcopysign) => Some(RuntimeFunction::F32Copysign),
        (F64, Opcode::Fadd) => Some(RuntimeFunction::F64Add),
        (F64, Opcode::Fsub) => Some(RuntimeFunction::F64Sub),
        (F64, Opcode::Fmul) => Some(RuntimeFunction::F64Mul),
        (F64, Opcode::Fdiv) => Some(RuntimeFunction::F64Div),
        (F64, Opcode::Fmin) => Some(RuntimeFunction::F64Min),
        (F64, Opcode::Fmax) => Some(RuntimeFunction::F64Max),
        (F64, Opcode::Fcopysign) => Some(RuntimeFunction::F64Copysign),
        _ => None,
    }
}

pub(super) const fn binary_imm_function(
    op: Opcode, ty: cranelift_codegen::ir::Type,
) -> Option<RuntimeFunction> {
    match (ty, op) {
        (I32, Opcode::IaddImm) => Some(RuntimeFunction::I32Add),
        (I32, Opcode::ImulImm) => Some(RuntimeFunction::I32Mul),
        (I32, Opcode::BandImm) => Some(RuntimeFunction::I32Band),
        (I32, Opcode::BorImm) => Some(RuntimeFunction::I32Bor),
        (I32, Opcode::BxorImm) => Some(RuntimeFunction::I32Bxor),
        (I32, Opcode::IshlImm) => Some(RuntimeFunction::I32Shl),
        (I32, Opcode::UshrImm) => Some(RuntimeFunction::I32ShrU),
        (I32, Opcode::SshrImm) => Some(RuntimeFunction::I32ShrS),
        (I64, Opcode::IaddImm) => Some(RuntimeFunction::I64Add),
        (I64, Opcode::ImulImm) => Some(RuntimeFunction::I64Mul),
        (I64, Opcode::BandImm) => Some(RuntimeFunction::I64Band),
        (I64, Opcode::BorImm) => Some(RuntimeFunction::I64Bor),
        (I64, Opcode::BxorImm) => Some(RuntimeFunction::I64Bxor),
        (I64, Opcode::IshlImm) => Some(RuntimeFunction::I64Shl),
        (I64, Opcode::UshrImm) => Some(RuntimeFunction::I64ShrU),
        (I64, Opcode::SshrImm) => Some(RuntimeFunction::I64ShrS),
        _ => None,
    }
}
