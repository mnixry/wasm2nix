use cranelift_codegen::{
    ir,
    ir::{
        Opcode,
        types::{F32, F64, I32, I64},
    },
};
use cranelift_wasm::WasmValType;
use wasm2nix_runtime::RuntimeFunction;

use super::error::{
    Result, UnsupportedGlobalTypeSnafu, UnsupportedLoadSnafu, UnsupportedStoreSnafu,
};

pub fn load_function(op: Opcode, ty: ir::Type) -> Result<RuntimeFunction> {
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

pub fn store_function(op: Opcode, ty: ir::Type) -> Result<RuntimeFunction> {
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

pub fn global_get_function(ty: WasmValType) -> Result<RuntimeFunction> {
    match ty {
        WasmValType::I32 | WasmValType::Ref(_) => Ok(RuntimeFunction::GlobalGetI32),
        WasmValType::I64 => Ok(RuntimeFunction::GlobalGetI64),
        WasmValType::F32 => Ok(RuntimeFunction::GlobalGetF32),
        WasmValType::F64 => Ok(RuntimeFunction::GlobalGetF64),
        WasmValType::V128 => UnsupportedGlobalTypeSnafu { ty: ty.to_string() }.fail(),
    }
}

pub fn global_set_function(ty: WasmValType) -> Result<RuntimeFunction> {
    match ty {
        WasmValType::I32 | WasmValType::Ref(_) => Ok(RuntimeFunction::GlobalSetI32),
        WasmValType::I64 => Ok(RuntimeFunction::GlobalSetI64),
        WasmValType::F32 => Ok(RuntimeFunction::GlobalSetF32),
        WasmValType::F64 => Ok(RuntimeFunction::GlobalSetF64),
        WasmValType::V128 => UnsupportedGlobalTypeSnafu { ty: ty.to_string() }.fail(),
    }
}
