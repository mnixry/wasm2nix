use cranelift_wasm::WasmValType;
use snafu::OptionExt;
use wasm2nix_runtime::RuntimeNamespace;
use wasm2nix_runtime::intrinsics::global_accessors;

use crate::lower::error::Result;
use crate::lower::error::UnsupportedGlobalTypeSnafu;
use crate::lower::names::func_name;
use crate::nix::AttrBind;
use crate::nix::AttrKey;
use crate::nix::Expr;
use crate::nix::nix;
use crate::translator::NixModule;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExportKeyMode {
    Public,
    Encoded,
}

pub(super) fn lower_exports_attrset(module: &NixModule, mode: ExportKeyMode) -> Result<Expr> {
    let info = &module.info;
    let mut binds = Vec::new();
    for (func_index, func) in &info.functions {
        let value = Expr::select(nix!(funcs), AttrKey::ident(func_name(func_index)));
        push_exports(&mut binds, &func.export_names, mode, &value);
    }
    for memory in info.memories.values() {
        push_exports(
            &mut binds,
            &memory.export_names,
            mode,
            &lower_memory_export(),
        );
    }
    for (table_index, table) in &info.tables {
        let value = lower_table_export(table_index.as_u32());
        push_exports(&mut binds, &table.export_names, mode, &value);
    }
    for (global_index, global) in &info.globals {
        if global.export_names.is_empty() {
            continue;
        }
        let value = lower_global_export(global_index.as_u32(), global.entity.wasm_ty)?;
        push_exports(&mut binds, &global.export_names, mode, &value);
    }
    Ok(Expr::attrset(binds))
}

/// Binds `value` under each export name; keys are strings because export
/// names need not be identifiers.
fn push_exports(binds: &mut Vec<AttrBind>, names: &[String], mode: ExportKeyMode, value: &Expr) {
    for name in names {
        if let Some(key) = export_attr_key(name, mode) {
            binds.push(AttrBind::new(AttrKey::string(key), value.clone()));
        }
    }
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

/// The only memory; `make_heap` rejects others.
fn lower_memory_export() -> Expr {
    nix!(merge(#{RuntimeNamespace::Mem.expr()}, { get = st: st.mem; }))
}

fn lower_table_export(table_index: u32) -> Expr {
    let table_key = table_index.to_string();
    nix!(merge(#{RuntimeNamespace::Table.expr()}, { get = st: st.tables.(table_key); }))
}

fn lower_global_export(global_index: u32, ty: WasmValType) -> Result<Expr> {
    let accessors = global_accessors(&ty).context(UnsupportedGlobalTypeSnafu { ty })?;
    let index = i64::from(global_index);
    let get_call = accessors
        .get
        .runtime_function()
        .call([Expr::int(index), nix!(st)]);
    let set_call = accessors
        .set
        .runtime_function()
        .call([Expr::int(index), nix!(value), nix!(st)]);
    Ok(nix!({
        get = st: #{get_call};
        set = value: st: #{set_call};
    }))
}
