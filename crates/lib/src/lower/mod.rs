//! Lowering of translated Wasm modules to Nix expressions.

mod const_expr;
mod error;
mod exports;
mod funcs;
mod function;
mod imports;
mod inst;
mod state;
#[cfg(test)]
mod tests;

pub use error::{LowerError, Result};

use crate::{
    lower::{
        exports::{ExportKeyMode, lower_exports_attrset},
        funcs::{lower_func_registry, lower_funcs_rec_attrset, lower_module_id, lower_start_state},
        state::lower_initial_state,
    },
    nix::{Expr, nix},
    translator::NixModule,
};

/// Lowers the translated module metadata and functions to a Nix expression.
///
/// # Errors
///
/// Returns an error when a Wasm construct is malformed or not yet supported by
/// the lowering layer.
pub fn lower_module(module: &NixModule) -> Result<Expr> {
    // Top-level:
    // rt: imports:
    // let moduleId = ...; funcs = rec { ... }; funcRegistry = ...; st0 = ...;
    // st = ...; exports = ...; in exports // { instantiate = args: { st =
    // st; exports = exports; }; __wasm2nix = { funcs = funcs; }; }
    let funcs_expr = lower_funcs_rec_attrset(module)?;
    let exports_expr = lower_exports_attrset(module, ExportKeyMode::Public)?;
    let exports_by_name_expr = lower_exports_attrset(module, ExportKeyMode::Encoded)?;
    let st0_expr = lower_initial_state(module)?;
    let instantiate_expr = nix!(args: { st = st; exports = exports; });
    let body = nix!(merge(exports, {
        instantiate = #{instantiate_expr};
        __wasm2nix = {
            funcs = funcs;
            moduleId = moduleId;
            st0 = st0;
            exportsByName = exportsByName;
        };
    }));

    Ok(nix!(rt: imports: let
        moduleId = #{lower_module_id()};
        funcs = #{funcs_expr};
        funcRegistry = #{lower_func_registry()};
        st0 = #{st0_expr};
        st = #{lower_start_state(module)};
        exports = #{exports_expr};
        exportsByName = #{exports_by_name_expr};
    in #{body}))
}

fn lambda_chain(params: &[String], body: Expr) -> Expr {
    params
        .iter()
        .rev()
        .fold(body, |acc, p| Expr::lambda(p.clone(), acc))
}
