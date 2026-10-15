use cranelift_wasm::WasmValType;
use wasm2nix_runtime::{RuntimeFunction, RuntimeNamespace};

use crate::{
    lower::error::{Result, UnsupportedGlobalTypeSnafu},
    nix::{AttrKey, Expr, nix},
    translator::NixModule,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExportKeyMode {
    Public,
    Encoded,
}

pub(super) fn lower_exports_attrset(module: &NixModule, mode: ExportKeyMode) -> Result<Expr> {
    let mut binds: Vec<(AttrKey, Expr)> = Vec::new();

    for (func_index, func_decl) in &module.info.functions {
        if func_decl.export_names.is_empty() {
            continue;
        }
        let f_name = format!("f{}", func_index.as_u32());
        let f_expr = Expr::select(nix!(funcs), AttrKey::ident(f_name));
        for export_name in &func_decl.export_names {
            let Some(key) = export_attr_key(export_name, mode) else {
                continue;
            };
            // Use string keys for safety (names can contain characters invalid
            // in identifiers).
            binds.push((AttrKey::string(key), f_expr.clone()));
        }
    }

    for (memory_index, memory_decl) in &module.info.memories {
        for export_name in &memory_decl.export_names {
            let Some(key) = export_attr_key(export_name, mode) else {
                continue;
            };
            binds.push((
                AttrKey::string(key),
                lower_memory_export(memory_index.as_u32()),
            ));
        }
    }

    for (table_index, table_decl) in &module.info.tables {
        for export_name in &table_decl.export_names {
            let Some(key) = export_attr_key(export_name, mode) else {
                continue;
            };
            binds.push((
                AttrKey::string(key),
                lower_table_export(table_index.as_u32()),
            ));
        }
    }

    for (global_index, global_decl) in &module.info.globals {
        for export_name in &global_decl.export_names {
            let Some(key) = export_attr_key(export_name, mode) else {
                continue;
            };
            binds.push((
                AttrKey::string(key),
                lower_global_export(global_index.as_u32(), global_decl.entity.wasm_ty)?,
            ));
        }
    }

    Ok(Expr::attrset(false, binds))
}

fn export_attr_key(export_name: &str, mode: ExportKeyMode) -> Option<String> {
    match mode {
        ExportKeyMode::Public if export_name.contains('\0') => None,
        ExportKeyMode::Public => Some(export_name.into()),
        ExportKeyMode::Encoded => Some(format!(
            "__wasm2nix_export_{}",
            hex::encode(export_name.as_bytes())
        )),
    }
}

fn lower_memory_export(memory_index: u32) -> Expr {
    debug_assert_eq!(memory_index, 0);
    nix!(merge(#{RuntimeNamespace::Mem.expr()}, { get = st: st.mem; }))
}

fn lower_table_export(table_index: u32) -> Expr {
    let table_key = table_index.to_string();
    nix!(merge(#{RuntimeNamespace::Table.expr()}, { get = st: st.tables.(table_key); }))
}

fn lower_global_export(global_index: u32, ty: WasmValType) -> Result<Expr> {
    let get = global_get_function(ty)?;
    let set = global_set_function(ty)?;
    let index = i64::from(global_index);
    let get_call = get.call(vec![Expr::int(index), nix!(st)]);
    let set_call = set.call(vec![Expr::int(index), nix!(value), nix!(st)]);
    Ok(nix!({
        get = st: #{get_call};
        set = value: st: #{set_call};
    }))
}

fn global_get_function(ty: WasmValType) -> Result<RuntimeFunction> {
    match ty {
        WasmValType::I32 | WasmValType::Ref(_) => Ok(RuntimeFunction::GlobalGetI32),
        WasmValType::I64 => Ok(RuntimeFunction::GlobalGetI64),
        WasmValType::F32 => Ok(RuntimeFunction::GlobalGetF32),
        WasmValType::F64 => Ok(RuntimeFunction::GlobalGetF64),
        WasmValType::V128 => UnsupportedGlobalTypeSnafu { ty: ty.to_string() }.fail(),
    }
}

fn global_set_function(ty: WasmValType) -> Result<RuntimeFunction> {
    match ty {
        WasmValType::I32 | WasmValType::Ref(_) => Ok(RuntimeFunction::GlobalSetI32),
        WasmValType::I64 => Ok(RuntimeFunction::GlobalSetI64),
        WasmValType::F32 => Ok(RuntimeFunction::GlobalSetF32),
        WasmValType::F64 => Ok(RuntimeFunction::GlobalSetF64),
        WasmValType::V128 => UnsupportedGlobalTypeSnafu { ty: ty.to_string() }.fail(),
    }
}
