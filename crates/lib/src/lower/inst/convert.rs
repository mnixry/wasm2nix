use cranelift_codegen::ir::{
    self, Opcode,
    types::{F32, F64, I8, I16, I32, I64},
};
use wasm2nix_runtime::RuntimeFunction;

use crate::{
    lower::{
        error::Result,
        inst::{InstCtx, LoweredInst},
    },
    nix::{Expr, nix},
};

impl InstCtx<'_> {
    pub(super) fn lower_bitcast(&self) -> LoweredInst {
        let arg = self.data.arguments(&self.func.dfg.value_lists)[0];
        self.bind_result(self.value_expr(arg))
    }

    pub(super) fn lower_uextend(&self, arg: ir::Value) -> LoweredInst {
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

    pub(super) fn lower_sextend(&self, arg: ir::Value) -> LoweredInst {
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

    pub(super) fn lower_float_to_int(&self, arg: ir::Value) -> Result<LoweredInst> {
        let out = self.results[0];
        let in_ty = self.func.dfg.value_type(arg);
        let out_ty = self.func.dfg.value_type(out);
        let Some(function) = float_to_int_conv_function(self.op, in_ty, out_ty) else {
            return self.unsupported_opcode();
        };
        let e = function.call(vec![self.value_expr(arg)]);
        Ok(self.pure_bind(out, e))
    }

    pub(super) fn lower_int_to_float(&self, arg: ir::Value) -> Result<LoweredInst> {
        let out = self.results[0];
        let in_ty = self.func.dfg.value_type(arg);
        let out_ty = self.func.dfg.value_type(out);
        let Some(function) = int_to_float_conv_function(self.op, in_ty, out_ty) else {
            return self.unsupported_opcode();
        };
        let e = function.call(vec![self.value_expr(arg)]);
        Ok(self.pure_bind(out, e))
    }

    pub(super) fn lower_float_resize(&self, arg: ir::Value) -> LoweredInst {
        let function = if self.op == Opcode::Fpromote {
            RuntimeFunction::F64PromoteF32
        } else {
            RuntimeFunction::F32DemoteF64
        };
        self.bind_result(function.call(vec![self.value_expr(arg)]))
    }
}

const fn float_to_int_conv_function(
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

const fn int_to_float_conv_function(
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
