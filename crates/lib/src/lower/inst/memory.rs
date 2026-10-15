use cranelift_codegen::ir::{
    self, Opcode,
    types::{F32, F64, I32, I64},
};
use wasm2nix_runtime::RuntimeFunction;

use crate::{
    lower::{
        error::{Result, UnsupportedLoadSnafu, UnsupportedStoreSnafu},
        inst::{InstCtx, LoweredInst},
    },
    nix::Expr,
};

impl InstCtx<'_> {
    pub(super) fn lower_load(&self, arg: ir::Value, offset: i32) -> Result<LoweredInst> {
        let out = self.results[0];
        let out_ty = self.func.dfg.value_type(out);
        let addr = self.addr_with_offset(arg, offset);
        let e = Expr::app(
            load_function(self.op, out_ty)?.expr(),
            vec![addr, Expr::ident(self.st_name)],
        );
        Ok(self.pure_bind(out, e))
    }

    pub(super) fn lower_store(&self, args: [ir::Value; 2], offset: i32) -> Result<LoweredInst> {
        let val = self.value_expr(args[0]);
        let addr = self.addr_with_offset(args[1], offset);
        let e = Expr::app(
            store_function(self.op, self.func.dfg.value_type(args[0]))?.expr(),
            vec![val, addr, Expr::ident(self.st_name)],
        );
        Ok(self.stateful_state_step(e))
    }
}

fn load_function(op: Opcode, ty: ir::Type) -> Result<RuntimeFunction> {
    match op {
        Opcode::Load => match ty {
            I32 => Ok(RuntimeFunction::MemLoadI32),
            I64 => Ok(RuntimeFunction::MemLoadI64),
            F32 => Ok(RuntimeFunction::MemLoadF32),
            F64 => Ok(RuntimeFunction::MemLoadF64),
            _ => UnsupportedLoadSnafu {
                opcode: op.to_string(),
                ty: ty.to_string(),
            }
            .fail(),
        },
        Opcode::Uload8 => Ok(RuntimeFunction::MemLoadU8),
        Opcode::Sload8 if ty == I64 => Ok(RuntimeFunction::MemLoadS8I64),
        Opcode::Sload8 => Ok(RuntimeFunction::MemLoadS8),
        Opcode::Uload16 => Ok(RuntimeFunction::MemLoadU16),
        Opcode::Sload16 if ty == I64 => Ok(RuntimeFunction::MemLoadS16I64),
        Opcode::Sload16 => Ok(RuntimeFunction::MemLoadS16),
        Opcode::Uload32 => Ok(RuntimeFunction::MemLoadU32),
        Opcode::Sload32 if ty == I64 => Ok(RuntimeFunction::MemLoadS32I64),
        Opcode::Sload32 => Ok(RuntimeFunction::MemLoadS32),
        _ => UnsupportedLoadSnafu {
            opcode: op.to_string(),
            ty: ty.to_string(),
        }
        .fail(),
    }
}

fn store_function(op: Opcode, ty: ir::Type) -> Result<RuntimeFunction> {
    match op {
        Opcode::Store => match ty {
            I32 => Ok(RuntimeFunction::MemStoreI32),
            I64 => Ok(RuntimeFunction::MemStoreI64),
            F32 => Ok(RuntimeFunction::MemStoreF32),
            F64 => Ok(RuntimeFunction::MemStoreF64),
            _ => UnsupportedStoreSnafu {
                opcode: op.to_string(),
                ty: ty.to_string(),
            }
            .fail(),
        },
        Opcode::Istore8 => Ok(RuntimeFunction::MemStoreU8),
        Opcode::Istore16 => Ok(RuntimeFunction::MemStoreU16),
        Opcode::Istore32 => Ok(RuntimeFunction::MemStoreI32),
        _ => UnsupportedStoreSnafu {
            opcode: op.to_string(),
            ty: ty.to_string(),
        }
        .fail(),
    }
}
