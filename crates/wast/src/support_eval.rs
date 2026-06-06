use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    process::Command,
};

use serde_json::Value as JsonValue;
use snafu::prelude::*;
use wasm2nix::nix::{AttrKey, Expr, nix};
use wast::WastArg;

use crate::{
    error::{
        DecodeEvalJsonSnafu, EvalNixFailedSnafu, EvalNixSnafu, NixIntegerOverflowSnafu, Result,
        UnknownModuleSnafu,
    },
    support_core::{HarnessContext, HarnessModule},
    support_state::{encoded_export_key, json_to_nix_expr, wast_arg_to_nix},
};

pub struct InvokeResult {
    pub results: JsonValue,
    pub state: String,
}

pub fn eval_instantiate(
    ctx: &HarnessContext, module: &Path, module_key: &str,
    modules: &HashMap<String, HarnessModule>, aliases: &HashMap<String, String>,
) -> Result<String> {
    let world = EvalWorld::new(modules, aliases);
    let imports = imports_expr(module_key, aliases, &world.module_vars);
    let expr = Expr::let_in(
        eval_binds(
            world,
            vec![("m_new".into(), import_module_expr(module, imports))],
        ),
        nix!(let i = m_new.instantiate({}); in i.st),
    );
    let value = eval_nix_json(ctx, &expr)?;
    json_to_nix_expr(&value)
}

pub fn eval_initial_state(
    ctx: &HarnessContext, module: &Path, module_key: &str,
    modules: &HashMap<String, HarnessModule>, aliases: &HashMap<String, String>,
) -> Result<String> {
    let world = EvalWorld::new(modules, aliases);
    let imports = imports_expr(module_key, aliases, &world.module_vars);
    let expr = Expr::let_in(
        eval_binds(
            world,
            vec![("m_new".into(), import_module_expr(module, imports))],
        ),
        nix!(m_new."__wasm2nix"."st0"),
    );
    let value = eval_nix_json(ctx, &expr)?;
    json_to_nix_expr(&value)
}

pub fn eval_invoke(
    ctx: &HarnessContext, module_key: &str, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, name: &str, args: &[WastArg<'_>], state: Option<&str>,
) -> Result<InvokeResult> {
    let args = args
        .iter()
        .map(wast_arg_to_nix)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .chain(std::iter::once(Expr::ident("st_in")))
        .collect::<Vec<_>>();
    let state_expr = state.map_or_else(|| nix!(i.st), state_expr);
    let world = EvalWorld::new(modules, aliases);
    let module_var = world
        .module_vars
        .get(module_key)
        .context(UnknownModuleSnafu {
            key: module_key.to_string(),
        })?
        .clone();
    let export_key = encoded_export_key(name);
    let export =
        nix!(#{Expr::ident(module_var.as_str())}."__wasm2nix"."exportsByName".(export_key));
    let expr = Expr::let_in(
        eval_binds(
            world,
            vec![
                (
                    "i".into(),
                    nix!(#{Expr::ident(module_var.as_str())}.instantiate({})),
                ),
                ("st_in".into(), state_expr),
                ("call".into(), Expr::app(export, args)),
            ],
        ),
        nix!({
            results = call.results;
            st = call.st;
        }),
    );
    let value = eval_nix_json(ctx, &expr)?;
    let results = value["results"].clone();
    let state = json_to_nix_expr(&value["st"])?;
    Ok(InvokeResult { results, state })
}

pub fn eval_get(
    ctx: &HarnessContext, module_key: &str, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, name: &str, state: Option<&str>,
) -> Result<InvokeResult> {
    let state_expr = state.map_or_else(|| nix!(i.st), state_expr);
    let world = EvalWorld::new(modules, aliases);
    let module_var = world
        .module_vars
        .get(module_key)
        .context(UnknownModuleSnafu {
            key: module_key.to_string(),
        })?
        .clone();
    let export_key = encoded_export_key(name);
    let export =
        nix!(#{Expr::ident(module_var.as_str())}."__wasm2nix"."exportsByName".(export_key));
    let expr = Expr::let_in(
        eval_binds(
            world,
            vec![
                (
                    "i".into(),
                    nix!(#{Expr::ident(module_var.as_str())}.instantiate({})),
                ),
                ("st_in".into(), state_expr),
                ("value".into(), nix!(#{export}.get(st_in))),
            ],
        ),
        nix!({
            results = [value];
            st = st_in;
        }),
    );
    let value = eval_nix_json(ctx, &expr)?;
    let results = value["results"].clone();
    let state = json_to_nix_expr(&value["st"])?;
    Ok(InvokeResult { results, state })
}

pub fn eval_nix_json(ctx: &HarnessContext, expr: &Expr) -> Result<JsonValue> {
    let expr = nix!(let harness = #{harness_import_expr(ctx)}; in harness.toJSON(#{expr}));
    let rendered = expr.render();
    let path = ctx.write_temp_nix_file("eval", &rendered)?;
    let output = Command::new("nix")
        .args(["--extra-experimental-features", "nix-command flakes"])
        .args(["eval", "--store", "dummy://", "--raw", "--impure", "-f"])
        .arg(&path)
        .output()
        .context(EvalNixSnafu)?;
    if !output.status.success() {
        return EvalNixFailedSnafu {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
        .fail();
    }
    let value = serde_json::from_slice(&output.stdout).context(DecodeEvalJsonSnafu)?;
    Ok(value)
}

fn rt_import_expr() -> Expr {
    import_nix_file_expr(wasm2nix_runtime::runtime_nix_path())
}

fn harness_import_expr(ctx: &HarnessContext) -> Expr {
    nix!(import(#{Expr::path(crate::harness_nix_path().display().to_string())}, {
        tracePrints = #{ctx.options.trace_prints};
    }))
}

fn eval_binds(world: EvalWorld, extra: Vec<(String, Expr)>) -> Vec<(String, Expr)> {
    let mut binds = Vec::with_capacity(world.binds.len() + extra.len() + 1);
    binds.push(("rt".into(), rt_import_expr()));
    binds.extend(world.binds);
    binds.extend(extra);
    binds
}

fn import_nix_file_expr(path: &Path) -> Expr {
    nix!(import(#{Expr::path(path.display().to_string())}, {}))
}

fn import_module_expr(path: &Path, imports: Expr) -> Expr {
    nix!(import(#{Expr::path(path.display().to_string())}, rt, #{imports}))
}

fn harness_attr(name: &str) -> Expr {
    nix!(harness.(name))
}

fn select_string_path(mut expr: Expr, attrs: &[&str]) -> Expr {
    for attr in attrs {
        expr = Expr::select(expr, AttrKey::string(*attr));
    }
    expr
}

pub fn state_expr(state: &str) -> Expr {
    Expr::raw(state.to_string())
}

pub fn nix_u64(value: u64) -> Result<Expr> {
    let value = i64::try_from(value).map_err(|_| NixIntegerOverflowSnafu { value }.build())?;
    Ok(Expr::int(value))
}

pub fn nix_optional_u64(value: Option<u64>) -> Result<Expr> {
    value
        .map(nix_u64)
        .transpose()
        .map(|value| value.unwrap_or_else(Expr::null))
}

struct EvalWorld {
    binds: Vec<(String, Expr)>,
    module_vars: HashMap<String, String>,
}

impl EvalWorld {
    fn new(modules: &HashMap<String, HarnessModule>, aliases: &HashMap<String, String>) -> Self {
        let mut keys = modules.keys().cloned().collect::<Vec<_>>();
        keys.sort();

        let raw_module_vars = keys
            .iter()
            .enumerate()
            .map(|(i, key)| (key.clone(), format!("m{i}")))
            .collect::<HashMap<_, _>>();
        let module_vars = keys
            .iter()
            .enumerate()
            .map(|(i, key)| (key.clone(), format!("m{i}_import")))
            .collect::<HashMap<_, _>>();

        let mut binds = Vec::new();
        for (i, key) in keys.iter().enumerate() {
            let module = &modules[key];
            let raw_var = &raw_module_vars[key];
            let import_var = &module_vars[key];
            let state_var = format!("m{i}_state");
            let imports = imports_expr(key, aliases, &module_vars);
            binds.push((raw_var.clone(), import_module_expr(&module.path, imports)));
            let module_state_expr = module.state.as_deref().map_or_else(
                || nix!((#{Expr::ident(raw_var.as_str())}.instantiate({})).st),
                state_expr,
            );
            binds.push((state_var.clone(), module_state_expr));
            binds.push((
                import_var.clone(),
                nix!(merge(#{Expr::ident(raw_var.as_str())}, {
                    "__wasm2nixState" = #{Expr::ident(state_var.as_str())};
                })),
            ));
        }

        binds.push((
            "funcRegistry".into(),
            Expr::attrset(
                false,
                keys.iter()
                    .map(|key| {
                        (
                            AttrKey::string(key.as_str()),
                            select_string_path(
                                Expr::ident(module_vars[key].as_str()),
                                &["__wasm2nix", "funcs"],
                            ),
                        )
                    })
                    .collect(),
            ),
        ));

        Self { binds, module_vars }
    }
}

fn imports_expr(
    module_key: &str, aliases: &HashMap<String, String>, module_vars: &HashMap<String, String>,
) -> Expr {
    let mut fields = BTreeMap::new();
    fields.insert("__moduleId".to_string(), Expr::string(module_key));
    fields.insert("__funcRegistry".to_string(), Expr::ident("funcRegistry"));
    fields.insert("spectest".to_string(), harness_attr("spectest"));

    for (key, var) in module_vars {
        fields.insert(key.clone(), Expr::ident(var.as_str()));
    }
    for (alias, key) in aliases {
        if let Some(var) = module_vars.get(key) {
            fields.insert(alias.clone(), Expr::ident(var.as_str()));
        }
    }

    Expr::attrset(
        false,
        fields
            .into_iter()
            .map(|(name, value)| (AttrKey::string(name), value))
            .collect(),
    )
}
