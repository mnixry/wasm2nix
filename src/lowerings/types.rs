use cranelift_codegen::{
    ir,
    ir::{
        Opcode,
        types::{F32, F64, I32, I64},
    },
};
use cranelift_wasm::WasmValType;

use super::{
    error::{
        Result, UnsupportedClifTypeSnafu, UnsupportedGlobalTypeSnafu, UnsupportedLoadSnafu,
        UnsupportedStoreSnafu,
    },
    rt_fn,
};
use crate::nix::Expr;

pub(crate) fn int_type_key(ty: ir::Type, context: &'static str) -> Result<&'static str> {
    if ty == I32 {
        Ok("i32")
    } else if ty == I64 {
        Ok("i64")
    } else {
        UnsupportedClifTypeSnafu {
            context,
            ty: ty.to_string(),
        }
        .fail()
    }
}

pub(crate) fn float_type_key(ty: ir::Type, context: &'static str) -> Result<&'static str> {
    if ty == F32 {
        Ok("f32")
    } else if ty == F64 {
        Ok("f64")
    } else {
        UnsupportedClifTypeSnafu {
            context,
            ty: ty.to_string(),
        }
        .fail()
    }
}

pub(crate) fn numeric_type_key(ty: ir::Type, context: &'static str) -> Result<&'static str> {
    if ty == I32 {
        Ok("i32")
    } else if ty == I64 {
        Ok("i64")
    } else if ty == F32 {
        Ok("f32")
    } else if ty == F64 {
        Ok("f64")
    } else {
        UnsupportedClifTypeSnafu {
            context,
            ty: ty.to_string(),
        }
        .fail()
    }
}

pub(crate) fn load_fn_name(op: Opcode, ty: ir::Type) -> Result<&'static str> {
    match op {
        Opcode::Load => match ty {
            I32 => Ok("load_i32"),
            I64 => Ok("load_i64"),
            F32 => Ok("load_f32"),
            F64 => Ok("load_f64"),
            _ => UnsupportedLoadSnafu {
                opcode: op.to_string(),
                ty: ty.to_string(),
            }
            .fail(),
        },
        Opcode::Uload8 => Ok("load_u8"),
        Opcode::Sload8 if ty == I64 => Ok("load_s8_i64"),
        Opcode::Sload8 => Ok("load_s8"),
        Opcode::Uload16 => Ok("load_u16"),
        Opcode::Sload16 if ty == I64 => Ok("load_s16_i64"),
        Opcode::Sload16 => Ok("load_s16"),
        Opcode::Uload32 => Ok("load_u32"),
        Opcode::Sload32 if ty == I64 => Ok("load_s32_i64"),
        Opcode::Sload32 => Ok("load_s32"),
        _ => UnsupportedLoadSnafu {
            opcode: op.to_string(),
            ty: ty.to_string(),
        }
        .fail(),
    }
}

pub(crate) fn store_fn_name(op: Opcode, ty: ir::Type) -> Result<&'static str> {
    match op {
        Opcode::Store => match ty {
            I32 => Ok("store_i32"),
            I64 => Ok("store_i64"),
            F32 => Ok("store_f32"),
            F64 => Ok("store_f64"),
            _ => UnsupportedStoreSnafu {
                opcode: op.to_string(),
                ty: ty.to_string(),
            }
            .fail(),
        },
        Opcode::Istore8 => Ok("store_u8"),
        Opcode::Istore16 => Ok("store_u16"),
        Opcode::Istore32 => Ok("store_i32"),
        _ => UnsupportedStoreSnafu {
            opcode: op.to_string(),
            ty: ty.to_string(),
        }
        .fail(),
    }
}

pub(crate) fn global_get_name(ty: WasmValType) -> Result<&'static str> {
    match ty {
        WasmValType::I32 => Ok("get_i32"),
        WasmValType::I64 => Ok("get_i64"),
        WasmValType::F32 => Ok("get_f32"),
        WasmValType::F64 => Ok("get_f64"),
        WasmValType::Ref(_) => Ok("get_i32"),
        WasmValType::V128 => UnsupportedGlobalTypeSnafu { ty: ty.to_string() }.fail(),
    }
}

pub(crate) fn global_set_name(ty: WasmValType) -> Result<&'static str> {
    match ty {
        WasmValType::I32 => Ok("set_i32"),
        WasmValType::I64 => Ok("set_i64"),
        WasmValType::F32 => Ok("set_f32"),
        WasmValType::F64 => Ok("set_f64"),
        WasmValType::Ref(_) => Ok("set_i32"),
        WasmValType::V128 => UnsupportedGlobalTypeSnafu { ty: ty.to_string() }.fail(),
    }
}

pub(crate) fn global_get_expr(ty: WasmValType) -> Result<Expr> {
    Ok(rt_fn(&["global", global_get_name(ty)?]))
}

pub(crate) fn global_set_expr(ty: WasmValType) -> Result<Expr> {
    Ok(rt_fn(&["global", global_set_name(ty)?]))
}
