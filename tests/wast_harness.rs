use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value as JsonValue;
use wasm2nix::translator::NixModule;
use wast::core::{NanPattern, WastArgCore, WastRetCore};
use wast::parser::{self, ParseBuffer};
use wast::{Wast, WastArg, WastDirective, WastExecute, WastRet};

static NEXT_FILE: AtomicUsize = AtomicUsize::new(0);

#[test]
#[ignore = "requires nix eval and currently covers the harness smoke path only"]
fn wast_harness_i32_assert_return_smoke() {
    run_with_large_stack(|| {
        let script = r#"
            (module
              (func (export "add") (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.add))
            (assert_return (invoke "add" (i32.const 40) (i32.const 2)) (i32.const 42))
        "#;

        run_wast_script(script).expect("wast smoke should pass");
    });
}

#[test]
#[ignore = "set WASM_SPEC_TEST to a .wast path and run with --ignored"]
fn wast_harness_single_file_from_env() {
    run_with_large_stack(|| {
        let path =
            std::env::var("WASM_SPEC_TEST").expect("WASM_SPEC_TEST must point at a .wast file");
        let script = fs::read_to_string(&path).expect("failed to read WASM_SPEC_TEST");
        run_wast_script(&script).expect("wast file should pass supported assertions");
    });
}

fn run_with_large_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name("wast-large-stack".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(f)
        .expect("failed to spawn WAST harness thread")
        .join()
        .expect("WAST harness thread panicked");
}

fn run_wast_script(script: &str) -> Result<(), String> {
    let buf = ParseBuffer::new(script).map_err(|e| e.to_string())?;
    let wast = parser::parse::<Wast<'_>>(&buf).map_err(|e| e.to_string())?;
    let mut modules: HashMap<String, HarnessModule> = HashMap::new();
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut current_module: Option<String> = None;
    let mut next_anon_module = 0usize;

    for directive in wast.directives {
        match directive {
            WastDirective::Module(mut module) => {
                let module_name = module.name().map(|id| id.name().to_string());
                let wasm = module.encode().map_err(|e| e.to_string())?;
                let translated = NixModule::translate(&wasm).map_err(|e| e.to_string())?;
                let imported_tables = translated.info.imported_tables.clone();
                let table_exports = table_exports(&translated);
                let nix = wasm2nix::clif_to_nix::lower_module(&translated).render();
                let path = write_generated_module(&nix)?;
                let key = module_name.unwrap_or_else(|| {
                    let key = format!("__anon{next_anon_module}");
                    next_anon_module += 1;
                    key
                });
                let state = eval_instantiate(&path, &key, &modules, &aliases)?;
                modules.insert(
                    key.clone(),
                    HarnessModule {
                        path,
                        state: Some(state),
                        imported_tables,
                        table_exports,
                    },
                );
                merge_imported_table_initializers(&key, &mut modules, &aliases)?;
                current_module = Some(key);
            }
            WastDirective::Register { name, module, .. } => {
                let key = select_module_key(module, current_module.as_deref(), &aliases)?;
                aliases.insert(name.to_string(), key);
            }
            WastDirective::AssertReturn { exec, results, .. } => {
                let actual = match exec {
                    WastExecute::Invoke(invoke) => {
                        let key =
                            select_module_key(invoke.module, current_module.as_deref(), &aliases)?;
                        let state = modules
                            .get(&key)
                            .ok_or_else(|| format!("unknown module {key}"))?
                            .state
                            .clone();
                        let actual = eval_invoke(
                            &key,
                            &modules,
                            &aliases,
                            invoke.name,
                            &invoke.args,
                            state.as_deref(),
                        )?;
                        modules
                            .get_mut(&key)
                            .ok_or_else(|| format!("unknown module {key}"))?
                            .state = Some(actual.state.clone());
                        actual
                    }
                    WastExecute::Get { module, global, .. } => {
                        let key = select_module_key(module, current_module.as_deref(), &aliases)?;
                        let state = modules
                            .get(&key)
                            .ok_or_else(|| format!("unknown module {key}"))?
                            .state
                            .clone();
                        eval_get(&key, &modules, &aliases, global, state.as_deref())?
                    }
                    WastExecute::Wat(_) => {
                        return Err(
                            "assert_return module execution is not supported by this harness slice"
                                .into(),
                        );
                    }
                };
                let expected = expected_results_json(&results)?;
                if !results_match(&results, &actual.results)? {
                    return Err(format!(
                        "assert_return mismatch: expected {expected}, got {}",
                        actual.results
                    ));
                }
            }
            WastDirective::AssertTrap { exec, .. } => match exec {
                WastExecute::Invoke(invoke) => {
                    let key =
                        select_module_key(invoke.module, current_module.as_deref(), &aliases)?;
                    let module = modules
                        .get(&key)
                        .ok_or_else(|| format!("unknown module {key}"))?;
                    if eval_invoke(
                        &key,
                        &modules,
                        &aliases,
                        invoke.name,
                        &invoke.args,
                        module.state.as_deref(),
                    )
                    .is_ok()
                    {
                        return Err(format!("assert_trap invoke {} did not trap", invoke.name));
                    }
                }
                WastExecute::Get { module, global, .. } => {
                    let key = select_module_key(module, current_module.as_deref(), &aliases)?;
                    let module = modules
                        .get(&key)
                        .ok_or_else(|| format!("unknown module {key}"))?;
                    if eval_get(&key, &modules, &aliases, global, module.state.as_deref()).is_ok() {
                        return Err(format!("assert_trap get {global} did not trap"));
                    }
                }
                WastExecute::Wat(_) => {}
            },
            WastDirective::Invoke(invoke) => {
                let key = select_module_key(invoke.module, current_module.as_deref(), &aliases)?;
                let state = modules
                    .get(&key)
                    .ok_or_else(|| format!("unknown module {key}"))?
                    .state
                    .clone();
                let actual = eval_invoke(
                    &key,
                    &modules,
                    &aliases,
                    invoke.name,
                    &invoke.args,
                    state.as_deref(),
                )?;
                modules
                    .get_mut(&key)
                    .ok_or_else(|| format!("unknown module {key}"))?
                    .state = Some(actual.state);
            }
            _ => {}
        }
    }

    Ok(())
}

struct HarnessModule {
    path: PathBuf,
    state: Option<String>,
    imported_tables: Vec<(String, String)>,
    table_exports: HashMap<String, u32>,
}

fn select_module_key(
    module: Option<wast::token::Id<'_>>,
    current: Option<&str>,
    aliases: &HashMap<String, String>,
) -> Result<String, String> {
    if let Some(module) = module {
        let name = module.name();
        Ok(aliases
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string()))
    } else {
        current
            .map(str::to_string)
            .ok_or_else(|| "module access before module".to_string())
    }
}

fn write_generated_module(nix: &str) -> Result<PathBuf, String> {
    write_temp_nix_file("wast", nix)
}

fn write_temp_nix_file(kind: &str, nix: &str) -> Result<PathBuf, String> {
    let id = NEXT_FILE.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let path = std::env::temp_dir().join(format!("wasm2nix-{kind}-{pid}-{nanos}-{id}.nix"));
    fs::write(&path, nix).map_err(|e| e.to_string())?;
    Ok(path)
}

fn table_exports(module: &NixModule) -> HashMap<String, u32> {
    module
        .info
        .tables
        .iter()
        .flat_map(|(idx, table)| {
            table
                .export_names
                .iter()
                .cloned()
                .map(move |name| (name, idx.as_u32()))
        })
        .collect()
}

struct InvokeResult {
    results: String,
    state: String,
}

fn eval_instantiate(
    module: &PathBuf,
    module_key: &str,
    modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) -> Result<String, String> {
    let world = EvalWorld::new(modules, aliases);
    let imports = imports_expr(module_key, aliases, &world.module_vars);
    let expr = format!(
        "let rt = {}; {} m_new = import {} rt {imports}; i = m_new.instantiate {{}}; in i.st",
        rt_import_expr()?,
        world.binds,
        module.display(),
    );
    let value = eval_nix_json(&expr)?;
    json_to_nix(&value)
}

fn eval_invoke(
    module_key: &str,
    modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
    name: &str,
    args: &[WastArg<'_>],
    state: Option<&str>,
) -> Result<InvokeResult, String> {
    let args = args
        .iter()
        .map(wast_arg_to_nix)
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    let invoke = if args.is_empty() {
        format!("i.exports.\"{name}\" st_in")
    } else {
        format!("i.exports.\"{name}\" {args} st_in")
    };
    let state_expr = state.unwrap_or("i.st");
    let world = EvalWorld::new(modules, aliases);
    let module_var = world
        .module_vars
        .get(module_key)
        .ok_or_else(|| format!("unknown module {module_key}"))?;
    let expr = format!(
        "let rt = {}; {} i = {module_var}.instantiate {{}}; st_in = {state_expr}; call = {invoke}; in {{ results = call.results; st = call.st; }}",
        rt_import_expr()?,
        world.binds
    );
    let value = eval_nix_json(&expr)?;
    let results =
        serde_json::to_string(&value["results"]).map_err(|e| format!("invalid results: {e}"))?;
    let state = json_to_nix(&value["st"])?;
    Ok(InvokeResult { results, state })
}

fn eval_get(
    module_key: &str,
    modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
    name: &str,
    state: Option<&str>,
) -> Result<InvokeResult, String> {
    let state_expr = state.unwrap_or("i.st");
    let world = EvalWorld::new(modules, aliases);
    let module_var = world
        .module_vars
        .get(module_key)
        .ok_or_else(|| format!("unknown module {module_key}"))?;
    let expr = format!(
        "let rt = {}; {} i = {module_var}.instantiate {{}}; st_in = {state_expr}; value = i.exports.\"{name}\".get st_in; in {{ results = [ value ]; st = st_in; }}",
        rt_import_expr()?,
        world.binds
    );
    let value = eval_nix_json(&expr)?;
    let results =
        serde_json::to_string(&value["results"]).map_err(|e| format!("invalid results: {e}"))?;
    let state = json_to_nix(&value["st"])?;
    Ok(InvokeResult { results, state })
}

fn eval_nix_json(expr: &str) -> Result<JsonValue, String> {
    let path = write_temp_nix_file("eval", expr)?;
    let output = Command::new("nix")
        .args(["eval", "--json", "--impure", "-f"])
        .arg(&path)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

fn rt_import_expr() -> Result<String, String> {
    let path = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join("nix/rt.nix");
    Ok(format!("import {} {{}}", path.display()))
}

struct EvalWorld {
    binds: String,
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

        let mut binds = String::new();
        for (i, key) in keys.iter().enumerate() {
            let module = &modules[key];
            let raw_var = &raw_module_vars[key];
            let import_var = &module_vars[key];
            let state_var = format!("m{i}_state");
            let imports = imports_expr(key, aliases, &module_vars);
            binds.push_str(&format!(
                "{raw_var} = import {} rt {imports}; ",
                module.path.display()
            ));
            let state_expr = module
                .state
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| format!("({raw_var}.instantiate {{}}).st"));
            binds.push_str(&format!("{state_var} = {state_expr}; "));
            binds.push_str(&format!(
                "{import_var} = {raw_var} // {{ \"__wasm2nixState\" = {state_var}; }}; "
            ));
        }

        binds.push_str("funcRegistry = { ");
        for key in &keys {
            let var = &module_vars[key];
            binds.push_str(&format!(
                "{} = {var}.\"__wasm2nix\".funcs; ",
                nix_string(key)
            ));
        }
        binds.push_str("}; ");

        Self { binds, module_vars }
    }
}

fn imports_expr(
    module_key: &str,
    aliases: &HashMap<String, String>,
    module_vars: &HashMap<String, String>,
) -> String {
    let mut fields = BTreeMap::new();
    fields.insert("__moduleId".to_string(), nix_string(module_key));
    fields.insert("__funcRegistry".to_string(), "funcRegistry".to_string());
    fields.insert("spectest".to_string(), spectest_imports_expr());

    for (key, var) in module_vars {
        fields.insert(key.clone(), var.clone());
    }
    for (alias, key) in aliases {
        if let Some(var) = module_vars.get(key) {
            fields.insert(alias.clone(), var.clone());
        }
    }

    let body = fields
        .into_iter()
        .map(|(name, value)| format!("{} = {value};", nix_string(&name)))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{{ {body} }}")
}

fn spectest_imports_expr() -> String {
    "{ global_i32 = 666; global_i64 = 666; global_f32 = 1143383654; global_f64 = 4649074691427585229; memory = { pageCount = 1; maxPageCount = 2; pages = { }; }; print = st: { results = [ ]; inherit st; }; print_i32 = _v: st: { results = [ ]; inherit st; }; print_i64 = _v: st: { results = [ ]; inherit st; }; print_f32 = _v: st: { results = [ ]; inherit st; }; print_f64 = _v: st: { results = [ ]; inherit st; }; print_i32_f32 = _i: _f: st: { results = [ ]; inherit st; }; print_f64_f64 = _a: _b: st: { results = [ ]; inherit st; }; }".to_string()
}

fn merge_imported_table_initializers(
    module_key: &str,
    modules: &mut HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) -> Result<(), String> {
    let imported_tables = modules
        .get(module_key)
        .ok_or_else(|| format!("unknown module {module_key}"))?
        .imported_tables
        .clone();

    for (source_table_index, (import_module, import_field)) in imported_tables.iter().enumerate() {
        let target_key = aliases
            .get(import_module)
            .cloned()
            .unwrap_or_else(|| import_module.clone());
        if !modules.contains_key(&target_key) {
            continue;
        }

        let target_table_index = modules[&target_key]
            .table_exports
            .get(import_field)
            .copied()
            .ok_or_else(|| {
                format!("imported table {import_module}.{import_field} has no table export")
            })?;
        let target_state = modules[&target_key]
            .state
            .clone()
            .ok_or_else(|| format!("target module {target_key} has no instantiated state"))?;
        let source_state = modules[module_key]
            .state
            .clone()
            .ok_or_else(|| format!("source module {module_key} has no instantiated state"))?;

        let merged = merge_table_state_expr(
            &target_state,
            target_table_index,
            &source_state,
            source_table_index as u32,
        );
        modules
            .get_mut(&target_key)
            .ok_or_else(|| format!("unknown module {target_key}"))?
            .state = Some(merged);
    }

    Ok(())
}

fn merge_table_state_expr(
    target_state: &str,
    target_table_index: u32,
    source_state: &str,
    source_table_index: u32,
) -> String {
    format!(
        "(let dstSt = {target_state}; srcSt = {source_state}; dstTables = dstSt.tables or {{}}; srcTables = srcSt.tables or {{}}; dstTable = dstTables.{} or {{ size = 0; elems = {{}}; }}; srcTable = srcTables.{} or {{ elems = {{}}; }}; mergedTable = dstTable // {{ elems = (dstTable.elems or {{}}) // (srcTable.elems or {{}}); }}; in dstSt // {{ tables = dstTables // {{ {} = mergedTable; }}; }})",
        nix_string(&target_table_index.to_string()),
        nix_string(&source_table_index.to_string()),
        nix_string(&target_table_index.to_string())
    )
}

fn nix_string(value: &str) -> String {
    format!("{value:?}")
}

fn wast_arg_to_nix(arg: &WastArg<'_>) -> Result<String, String> {
    match arg {
        WastArg::Core(WastArgCore::I32(value)) => Ok((*value as u32).to_string()),
        WastArg::Core(WastArgCore::I64(value)) => Ok(nix_int_literal(*value)),
        WastArg::Core(WastArgCore::F32(value)) => Ok(value.bits.to_string()),
        WastArg::Core(WastArgCore::F64(value)) => Ok(nix_int_literal(value.bits as i64)),
        WastArg::Core(WastArgCore::RefExtern(value)) => Ok(value.to_string()),
        WastArg::Core(WastArgCore::RefNull(_)) => Ok("0".to_string()),
        _ => Err("unsupported invoke argument for this harness slice".into()),
    }
}

fn nix_int_literal(value: i64) -> String {
    if value == i64::MIN {
        "(0 - 9223372036854775807 - 1)".to_string()
    } else if value < 0 {
        format!("({value})")
    } else {
        value.to_string()
    }
}

fn json_to_nix(value: &JsonValue) -> Result<String, String> {
    match value {
        JsonValue::Null => Ok("null".to_string()),
        JsonValue::Bool(value) => Ok(value.to_string()),
        JsonValue::Number(value) => Ok(nix_json_number(value)),
        JsonValue::String(value) => Ok(format!("{value:?}")),
        JsonValue::Array(items) => {
            let items = items
                .iter()
                .map(json_to_nix)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("[ {} ]", items.join(" ")))
        }
        JsonValue::Object(map) => {
            let fields = map
                .iter()
                .map(|(key, value)| Ok(format!("{key:?} = {};", json_to_nix(value)?)))
                .collect::<Result<Vec<_>, String>>()?;
            Ok(format!("{{ {} }}", fields.join(" ")))
        }
    }
}

fn nix_json_number(value: &serde_json::Number) -> String {
    let raw = value.to_string();
    if raw == "-9223372036854775808" {
        "(0 - 9223372036854775807 - 1)".to_string()
    } else if raw.starts_with('-') {
        format!("({raw})")
    } else {
        raw
    }
}

fn expected_results_json(results: &[WastRet<'_>]) -> Result<String, String> {
    let values = results
        .iter()
        .map(expected_result_to_json)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!("[{}]", values.join(",")))
}

fn expected_result_to_json(ret: &WastRet<'_>) -> Result<String, String> {
    match ret {
        WastRet::Core(WastRetCore::I32(value)) => Ok((*value as u32).to_string()),
        WastRet::Core(WastRetCore::I64(value)) => Ok(value.to_string()),
        WastRet::Core(WastRetCore::F32(NanPattern::Value(value))) => Ok(value.bits.to_string()),
        WastRet::Core(WastRetCore::F64(NanPattern::Value(value))) => {
            Ok((value.bits as i64).to_string())
        }
        WastRet::Core(WastRetCore::F32(NanPattern::CanonicalNan))
        | WastRet::Core(WastRetCore::F64(NanPattern::CanonicalNan)) => Ok("nan:canonical".into()),
        WastRet::Core(WastRetCore::F32(NanPattern::ArithmeticNan))
        | WastRet::Core(WastRetCore::F64(NanPattern::ArithmeticNan)) => Ok("nan:arithmetic".into()),
        WastRet::Core(WastRetCore::RefExtern(Some(value))) => Ok(value.to_string()),
        WastRet::Core(WastRetCore::RefNull(_)) => Ok("0".to_string()),
        _ => Err("unsupported assert_return result for this harness slice".into()),
    }
}

fn results_match(expected: &[WastRet<'_>], actual: &str) -> Result<bool, String> {
    let value: JsonValue =
        serde_json::from_str(actual).map_err(|e| format!("invalid actual results JSON: {e}"))?;
    let JsonValue::Array(actual_items) = value else {
        return Err("actual results must be a JSON array".into());
    };
    if actual_items.len() != expected.len() {
        return Ok(false);
    }
    for (expected, actual) in expected.iter().zip(actual_items.iter()) {
        if !result_matches(expected, actual)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn result_matches(expected: &WastRet<'_>, actual: &JsonValue) -> Result<bool, String> {
    match expected {
        WastRet::Core(WastRetCore::F32(NanPattern::CanonicalNan)) => {
            let bits = json_number_to_u64(actual)? as u32;
            Ok(f32_canonical_nan(bits))
        }
        WastRet::Core(WastRetCore::F32(NanPattern::ArithmeticNan)) => {
            let bits = json_number_to_u64(actual)? as u32;
            Ok(f32_arithmetic_nan(bits))
        }
        WastRet::Core(WastRetCore::F64(NanPattern::CanonicalNan)) => {
            let bits = json_number_to_u64(actual)?;
            Ok(f64_canonical_nan(bits))
        }
        WastRet::Core(WastRetCore::F64(NanPattern::ArithmeticNan)) => {
            let bits = json_number_to_u64(actual)?;
            Ok(f64_arithmetic_nan(bits))
        }
        _ => Ok(actual.to_string() == expected_result_to_json(expected)?),
    }
}

fn json_number_to_u64(value: &JsonValue) -> Result<u64, String> {
    let JsonValue::Number(number) = value else {
        return Err("expected numeric result".into());
    };
    if let Some(value) = number.as_u64() {
        Ok(value)
    } else if let Some(value) = number.as_i64() {
        Ok(value as u64)
    } else {
        Err("expected integer result".into())
    }
}

fn f32_canonical_nan(bits: u32) -> bool {
    (bits & 0x7f80_0000) == 0x7f80_0000 && (bits & 0x007f_ffff) == 0x0040_0000
}

fn f32_arithmetic_nan(bits: u32) -> bool {
    (bits & 0x7f80_0000) == 0x7f80_0000 && (bits & 0x0040_0000) != 0
}

fn f64_canonical_nan(bits: u64) -> bool {
    (bits & 0x7ff0_0000_0000_0000) == 0x7ff0_0000_0000_0000
        && (bits & 0x000f_ffff_ffff_ffff) == 0x0008_0000_0000_0000
}

fn f64_arithmetic_nan(bits: u64) -> bool {
    (bits & 0x7ff0_0000_0000_0000) == 0x7ff0_0000_0000_0000 && (bits & 0x0008_0000_0000_0000) != 0
}
