use cranelift_codegen::ir;

use crate::lower::error::Result;
use crate::lower::function::lower_function;
use crate::lower::imports::lower_imported_extern_state;
use crate::lower::lambda_chain;
use crate::lower::names::ST;
use crate::lower::names::func_name;
use crate::nix::AttrBind;
use crate::nix::AttrKey;
use crate::nix::Expr;
use crate::nix::nix;
use crate::translator::NixModule;

pub(super) fn lower_module_id() -> Expr {
    nix!(if builtins.hasAttr("__moduleId", imports) then imports."__moduleId" else "__self")
}

pub(super) fn lower_func_registry() -> Expr {
    nix!(merge(
        (if builtins.hasAttr("__funcRegistry", imports) then imports."__funcRegistry" else {}),
        builtins.listToAttrs([{ name = moduleId; value = funcs; }])
    ))
}

pub(super) fn lower_start_state(module: &NixModule) -> Expr {
    module.info.start_func.map_or_else(
        || nix!(st0),
        |start| {
            let start_func = Expr::select(nix!(funcs), AttrKey::ident(func_name(start)));
            nix!((#{start_func}(st0)).st)
        },
    )
}

pub(super) fn lower_funcs_rec_attrset(module: &NixModule) -> Result<Expr> {
    let info = &module.info;
    let imported = info.imported_funcs.iter().zip(info.functions.iter()).map(
        |((module_name, field_name), (func_index, func))| {
            let sig = &info.signatures[func.entity];
            let lowered = lower_imported_func(module_name, field_name, sig);
            AttrBind::new(AttrKey::ident(func_name(func_index)), lowered)
        },
    );
    let defined = info.function_bodies.iter().map(|(defined, func)| {
        let func_index = module.func_index(defined);
        let lowered = lower_function(module, func_index.as_u32(), func)?;
        Ok(AttrBind::new(
            AttrKey::ident(func_name(func_index)),
            lowered,
        ))
    });
    let binds = imported.map(Ok).chain(defined);
    Ok(Expr::rec_attrset(binds.collect::<Result<Vec<_>>>()?))
}

/// A function forwarding to an import, passing the importing module's state.
fn lower_imported_func(module_name: &str, field_name: &str, sig: &ir::Signature) -> Expr {
    let module = nix!(imports.(module_name));
    let imported = nix!(#{module.clone()}.(field_name));
    let mut params = (0..sig.params.len())
        .map(|idx| format!("arg{idx}"))
        .collect::<Vec<_>>();
    let mut call_args = params
        .iter()
        .map(|param| Expr::ident(param.as_str()))
        .collect::<Vec<_>>();
    call_args.push(lower_imported_extern_state(module));
    params.push(ST.into());

    let call = Expr::app(imported, call_args);
    let body = nix!(let call = #{call}; in { results = call.results; st = st; });
    lambda_chain(&params, body)
}
