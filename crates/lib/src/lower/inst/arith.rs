use cranelift_codegen::ir::{
    self, Opcode, Type,
    types::{F32, F64, I8, I32, I64},
};
use wasm2nix_runtime::RuntimeFunction;

use crate::{
    lower::{
        error::Result,
        inst::{InstCtx, LoweredInst},
    },
    nix::{AttrKey, BinOp, Expr, UnOp, nix},
};

impl InstCtx<'_> {
    pub(super) fn lower_iconst(&self, v: i64) -> LoweredInst {
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

    pub(super) fn lower_unary(&self, arg: ir::Value) -> Result<LoweredInst> {
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

    fn lower_int_unary(&self, arg: ir::Value) -> Result<LoweredInst> {
        let out = self.results[0];
        let out_ty = self.func.dfg.value_type(out);
        let Some(function) = int_unary_function(self.op, out_ty) else {
            return self.unsupported_opcode();
        };
        Ok(self.pure_bind(out, function.call(vec![self.value_expr(arg)])))
    }

    fn lower_float_unary(&self, arg: ir::Value) -> Result<LoweredInst> {
        let out = self.results[0];
        let out_ty = self.func.dfg.value_type(out);
        let Some(function) = float_unary_function(self.op, out_ty) else {
            return self.unsupported_opcode();
        };
        Ok(self.pure_bind(out, function.call(vec![self.value_expr(arg)])))
    }

    pub(super) fn lower_binary(&self, args: [ir::Value; 2]) -> Result<LoweredInst> {
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

        if let Some(e) = inline_int_binary(
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
        let Some(function) = binary_function(self.op, out_ty) else {
            return self.unsupported_opcode();
        };
        Ok(self.pure_bind(out, function.call(vec![a, b])))
    }

    pub(super) fn lower_binary_imm(&self, arg: ir::Value, imm64: i64) -> LoweredInst {
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
        if let Some(e) = inline_int_binary(
            self.op,
            out_ty,
            self.value_expr(arg),
            imm_expr.clone(),
            imm_const,
        ) {
            return self.pure_bind(out, e);
        }
        let Some(function) = binary_imm_function(self.op, out_ty) else {
            return self.unsupported(format!("wasm2nix: unimplemented binary-imm op {}", self.op));
        };
        self.pure_bind(out, function.call(vec![self.value_expr(arg), imm_expr]))
    }

    pub(super) fn lower_select(&self, args: [ir::Value; 3]) -> LoweredInst {
        let cond = self.truthy_expr(args[0]);
        let then_value = self.value_expr(args[1]);
        let else_value = self.value_expr(args[2]);
        self.bind_result(nix!(if #{cond} then #{then_value} else #{else_value}))
    }
}

const U32_MASK: i64 = 0xFFFF_FFFF;

/// Integer operations cheap enough to emit as inline Nix instead of a runtime
/// call. i32 values are canonical u32 and i64 values native int64, so bitwise
/// operations apply directly and i32 wraparound is a mask. `rhs_const` is the
/// right operand when it is a known constant.
fn inline_int_binary(
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

fn bit_op(name: &str, lhs: Expr, rhs: Expr) -> Expr {
    Expr::app(
        Expr::select(Expr::ident("builtins"), AttrKey::ident(name)),
        [lhs, rhs],
    )
}

/// Whether `expr` is one of the bitwise builtins emitted by
/// [`inline_int_binary`], which are pure on integers.
pub(in crate::lower) fn is_bit_op(expr: &Expr) -> bool {
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

const fn int_unary_function(
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

const fn float_unary_function(
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

const fn binary_function(op: Opcode, ty: cranelift_codegen::ir::Type) -> Option<RuntimeFunction> {
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

const fn binary_imm_function(
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
