use crate::nix::{Expr, nix};

/// An imported memory, table or global: either the value itself or an
/// accessor whose `get` reads it from the exporting module's state.
pub(super) fn lower_imported_extern(module_name: &str, field_name: &str) -> Expr {
    let module = nix!(imports.(module_name));
    let imported = nix!(#{module.clone()}.(field_name));
    nix!(if builtins.isAttrs(#{imported.clone()}) && builtins.hasAttr("get", #{imported.clone()})
        then #{imported.clone()}.get(#{lower_imported_extern_state(module)})
        else #{imported})
}

pub(super) fn lower_imported_extern_state(module: Expr) -> Expr {
    nix!(if builtins.isAttrs(#{module.clone()}) && builtins.hasAttr("__wasm2nixState", #{module.clone()})
        then #{module.clone()}."__wasm2nixState"
        else if builtins.isAttrs(#{module.clone()}) && builtins.hasAttr("instantiate", #{module.clone()})
            then (#{module}.instantiate({})).st
            else {})
}

pub(super) fn lower_attr_or(expr: Expr, attr: &str, default: Expr) -> Expr {
    nix!(if builtins.isAttrs(#{expr.clone()}) && builtins.hasAttr(#{attr}, #{expr.clone()})
        then #{expr}.(attr)
        else #{default})
}
