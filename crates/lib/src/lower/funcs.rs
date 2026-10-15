use cranelift_codegen::ir::{self};

use crate::{
    lower::{
        error::Result, function::lower_function, imports::lower_imported_extern_state, lambda_chain,
    },
    nix::{AttrKey, Expr, nix},
    translator::NixModule,
};

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
            let start_func =
                Expr::select(nix!(funcs), AttrKey::ident(format!("f{}", start.as_u32())));
            nix!((#{start_func}(st0)).st)
        },
    )
}

pub(super) fn lower_funcs_rec_attrset(module: &NixModule) -> Result<Expr> {
    let mut binds: Vec<(AttrKey, Expr)> = Vec::new();

    for (idx, (module_name, field_name)) in module.info.imported_funcs.iter().enumerate() {
        let func_index = cranelift_wasm::FuncIndex::from_u32(idx as u32);
        let sig_index = module.info.functions[func_index].entity;
        let imported =
            lower_imported_func(module_name, field_name, &module.info.signatures[sig_index]);
        binds.push((AttrKey::ident(format!("f{idx}")), imported));
    }

    for (defined_idx, func) in &module.info.function_bodies {
        let func_index = module.num_func_imports() as u32 + defined_idx.as_u32();
        let name = format!("f{func_index}");
        let lowered = lower_function(module, func_index, func)?;
        binds.push((AttrKey::ident(name), lowered));
    }

    Ok(Expr::attrset(true, binds))
}

fn lower_imported_func(module_name: &str, field_name: &str, sig: &ir::Signature) -> Expr {
    let module = nix!(imports.(module_name));
    let imported = nix!(#{module.clone()}.(field_name));
    let mut params = (0..sig.params.len())
        .map(|idx| format!("arg{idx}"))
        .collect::<Vec<_>>();
    params.push("st".to_string());

    let mut call_args = params
        .iter()
        .take(sig.params.len())
        .cloned()
        .map(Expr::ident)
        .collect::<Vec<_>>();
    call_args.push(lower_imported_extern_state(module));
    let call = Expr::app(imported, call_args);
    let body = nix!(let call = #{call}; in { results = call.results; st = st; });
    lambda_chain(&params, body)
}
