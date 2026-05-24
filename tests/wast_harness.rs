use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use cranelift_codegen::ir;
use cranelift_entity::EntityRef;
use serde_json::Value as JsonValue;
use tempfile::TempDir;
use wasm2nix::{
    nix::{AttrKey, BinOp, Expr},
    translator::NixModule,
};
use wast::{
    Wast, WastArg, WastDirective, WastExecute, WastRet,
    core::{NanPattern, WastArgCore, WastRetCore},
    lexer::Lexer,
    parser::{self, ParseBuffer},
};

#[test]
fn wast_harness_i32_assert_return_smoke() {
    let script = r#"
            (module
              (func (export "add") (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.add))
            (assert_return (invoke "add" (i32.const 40) (i32.const 2)) (i32.const 42))
        "#;

    run_wast_script(script).expect("wast smoke should pass");
}

fn run_wast_file_path(path: &str) {
    let path = PathBuf::from(path);
    run_with_large_stack(move || {
        let script = fs::read_to_string(&path).expect("failed to read WAST file");
        let label = path.display().to_string();
        run_wast_script(&script).unwrap_or_else(|err| panic!("{label}: {err}"));
    });
}

fn run_with_large_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name("wast-large-stack".into())
        .stack_size(256 * 1024 * 1024)
        .spawn(f)
        .expect("failed to spawn WAST harness thread")
        .join()
        .expect("WAST harness thread panicked");
}

struct HarnessContext {
    temp_dir: TempDir,
    next_file: usize,
}

impl HarnessContext {
    fn new() -> Result<Self, String> {
        Ok(Self {
            temp_dir: TempDir::new().map_err(|e| e.to_string())?,
            next_file: 0,
        })
    }

    fn write_temp_nix_file(&mut self, kind: &str, nix: &str) -> Result<PathBuf, String> {
        let path = self
            .temp_dir
            .path()
            .join(format!("{kind}-{}.nix", self.next_file));
        self.next_file += 1;
        fs::write(&path, nix).map_err(|e| e.to_string())?;
        Ok(path)
    }
}

fn run_wast_script(script: &str) -> Result<(), String> {
    let mut ctx = HarnessContext::new()?;
    let mut lexer = Lexer::new(script);
    lexer.allow_confusing_unicode(true);
    let buf = ParseBuffer::new_with_lexer(lexer).map_err(|e| e.to_string())?;
    let wast = parser::parse::<Wast<'_>>(&buf).map_err(|e| e.to_string())?;
    let mut modules: HashMap<String, HarnessModule> = HashMap::new();
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut current_module: Option<String> = None;
    let mut next_anon_module = 0usize;

    for (directive_index, directive) in wast.directives.into_iter().enumerate() {
        match directive {
            WastDirective::Module(mut module) => {
                let module_name = module.name().map(|id| id.name().to_string());
                let wasm = module.encode().map_err(|e| e.to_string())?;
                let translated = NixModule::translate(&wasm).map_err(|e| e.to_string())?;
                let imported_tables = translated.info.imported_tables.clone();
                let imported_globals = translated.info.imported_globals.clone();
                let imported_memories = translated.info.imported_memories.clone();
                validate_imports_for_linking(&mut ctx, &translated, &modules, &aliases)
                    .map_err(|e| format!("directive {directive_index}: {e}"))?;
                let func_exports = func_exports(&translated);
                let table_exports = table_exports(&translated);
                let global_exports = global_exports(&translated);
                let memory_exports = memory_exports(&translated);
                let table_export_limits = table_export_limits(&translated);
                let global_export_types = global_export_types(&translated);
                let memory_export_limits = memory_export_limits(&translated);
                if active_initializer_traps(&mut ctx, &translated, &modules, &aliases)? {
                    return Err(format!(
                        "directive {directive_index}: module instantiation trapped during active segment initialization"
                    ));
                }
                let nix = wasm2nix::clif_to_nix::lower_module(&translated).render();
                let path = write_generated_module(&mut ctx, &nix)?;
                let key = module_name.unwrap_or_else(|| {
                    let key = format!("__anon{next_anon_module}");
                    next_anon_module += 1;
                    key
                });
                let state = eval_instantiate(&mut ctx, &path, &key, &modules, &aliases)?;
                modules.insert(
                    key.clone(),
                    HarnessModule {
                        path,
                        state: Some(state),
                        imported_tables,
                        imported_globals,
                        imported_memories,
                        func_exports,
                        table_exports,
                        global_exports,
                        memory_exports,
                        table_export_limits,
                        global_export_types,
                        memory_export_limits,
                    },
                );
                merge_imported_table_initializers(&key, &mut modules, &aliases)?;
                sync_memory_imports_to_targets(&key, &mut modules, &aliases);
                sync_global_imports_from_targets(&mut modules, &aliases);
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
                            &mut ctx,
                            &key,
                            &modules,
                            &aliases,
                            invoke.name,
                            &invoke.args,
                            state.as_deref(),
                        )
                        .map_err(|e| {
                            format!(
                                "directive {directive_index}: invoke {} failed: {e}",
                                invoke.name
                            )
                        })?;
                        modules
                            .get_mut(&key)
                            .ok_or_else(|| format!("unknown module {key}"))?
                            .state = Some(actual.state.clone());
                        sync_global_imports_to_targets(&key, &mut modules, &aliases);
                        sync_memory_imports_to_targets(&key, &mut modules, &aliases);
                        sync_global_imports_from_targets(&mut modules, &aliases);
                        actual
                    }
                    WastExecute::Get { module, global, .. } => {
                        let key = select_module_key(module, current_module.as_deref(), &aliases)?;
                        sync_global_imports_from_targets(&mut modules, &aliases);
                        let state = modules
                            .get(&key)
                            .ok_or_else(|| format!("unknown module {key}"))?
                            .state
                            .clone();
                        eval_get(&mut ctx, &key, &modules, &aliases, global, state.as_deref())
                            .map_err(|e| format!("directive {directive_index}: {e}"))?
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
                        "directive {directive_index}: assert_return mismatch: expected {expected}, got {}",
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
                        &mut ctx,
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
                    if eval_get(
                        &mut ctx,
                        &key,
                        &modules,
                        &aliases,
                        global,
                        module.state.as_deref(),
                    )
                    .is_ok()
                    {
                        return Err(format!("assert_trap get {global} did not trap"));
                    }
                }
                WastExecute::Wat(mut wat) => {
                    apply_trapping_module_side_effects(
                        &mut ctx,
                        directive_index,
                        &mut wat,
                        &mut modules,
                        &aliases,
                    )?;
                }
            },
            WastDirective::AssertMalformed { mut module, .. }
            | WastDirective::AssertMalformedCustom { mut module, .. } => {
                let Ok(wasm) = module.encode() else {
                    continue;
                };
                if wasmparser::Validator::new_with_features(harness_wasm_features())
                    .validate_all(&wasm)
                    .is_err()
                {
                    continue;
                }
                if NixModule::translate(&wasm).is_err() {
                    continue;
                }
                return Err(format!(
                    "directive {directive_index}: assert_malformed module was accepted"
                ));
            }
            WastDirective::AssertInvalid { mut module, .. }
            | WastDirective::AssertInvalidCustom { mut module, .. } => {
                let Ok(wasm) = module.encode() else {
                    continue;
                };
                if NixModule::translate(&wasm).is_ok() {
                    return Err(format!(
                        "directive {directive_index}: assert_invalid module translated successfully"
                    ));
                }
            }
            WastDirective::AssertUnlinkable { mut module, .. } => {
                let wasm = module
                    .encode()
                    .map_err(|e| format!("directive {directive_index}: {e}"))?;
                let translated = NixModule::translate(&wasm)
                    .map_err(|e| format!("directive {directive_index}: {e}"))?;
                if validate_imports_for_linking(&mut ctx, &translated, &modules, &aliases).is_err()
                {
                    continue;
                }
                let nix = wasm2nix::clif_to_nix::lower_module(&translated).render();
                let path = write_generated_module(&mut ctx, &nix)?;
                let transient_key = format!("__unlinkable{directive_index}");
                if eval_instantiate(&mut ctx, &path, &transient_key, &modules, &aliases).is_ok() {
                    return Err(format!(
                        "directive {directive_index}: assert_unlinkable module instantiated successfully"
                    ));
                }
            }
            WastDirective::AssertExhaustion { call, .. } => {
                let key = select_module_key(call.module, current_module.as_deref(), &aliases)?;
                let module = modules
                    .get(&key)
                    .ok_or_else(|| format!("unknown module {key}"))?;
                if eval_invoke(
                    &mut ctx,
                    &key,
                    &modules,
                    &aliases,
                    call.name,
                    &call.args,
                    module.state.as_deref(),
                )
                .is_ok()
                {
                    return Err(format!(
                        "directive {directive_index}: assert_exhaustion invoke {} did not fail",
                        call.name
                    ));
                }
            }
            WastDirective::Invoke(invoke) => {
                let key = select_module_key(invoke.module, current_module.as_deref(), &aliases)?;
                let state = modules
                    .get(&key)
                    .ok_or_else(|| format!("unknown module {key}"))?
                    .state
                    .clone();
                let actual = eval_invoke(
                    &mut ctx,
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
                sync_global_imports_to_targets(&key, &mut modules, &aliases);
                sync_memory_imports_to_targets(&key, &mut modules, &aliases);
                sync_global_imports_from_targets(&mut modules, &aliases);
            }
            WastDirective::ModuleDefinition(_)
            | WastDirective::ModuleInstance { .. }
            | WastDirective::AssertException { .. }
            | WastDirective::AssertSuspension { .. }
            | WastDirective::Thread(_)
            | WastDirective::Wait { .. } => {
                return Err(format!(
                    "directive {directive_index}: unsupported WAST directive"
                ));
            }
        }
    }

    Ok(())
}

fn harness_wasm_features() -> wasmparser::WasmFeatures {
    wasmparser::WasmFeatures::WASM1
        .union(wasmparser::WasmFeatures::MUTABLE_GLOBAL)
        .union(wasmparser::WasmFeatures::SIGN_EXTENSION)
        .union(wasmparser::WasmFeatures::MULTI_VALUE)
        .union(wasmparser::WasmFeatures::REFERENCE_TYPES)
        .union(wasmparser::WasmFeatures::SATURATING_FLOAT_TO_INT)
        .union(wasmparser::WasmFeatures::BULK_MEMORY)
}

struct HarnessModule {
    path: PathBuf,
    state: Option<String>,
    imported_tables: Vec<(String, String)>,
    imported_globals: Vec<(String, String)>,
    imported_memories: Vec<(String, String)>,
    func_exports: HashMap<String, String>,
    table_exports: HashMap<String, u32>,
    global_exports: HashMap<String, u32>,
    memory_exports: HashMap<String, u32>,
    table_export_limits: HashMap<String, TableImportType>,
    global_export_types: HashMap<String, GlobalImportType>,
    memory_export_limits: HashMap<String, Limits>,
}

#[derive(Clone, Copy, Debug)]
struct Limits {
    min: u64,
    max: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GlobalImportType {
    ty: String,
    mutable: bool,
}

#[derive(Clone, Debug)]
enum ImportType {
    Func(String),
    Global(GlobalImportType),
    Memory(Limits),
    Table(TableImportType),
}

#[derive(Clone, Debug)]
struct TableImportType {
    ty: String,
    limits: Limits,
}

fn select_module_key(
    module: Option<wast::token::Id<'_>>, current: Option<&str>, aliases: &HashMap<String, String>,
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

fn validate_imports_for_linking(
    ctx: &mut HarnessContext, module: &NixModule, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) -> Result<(), String> {
    for (idx, (module_name, field_name)) in module.info.imported_funcs.iter().enumerate() {
        let func_index = cranelift_wasm::FuncIndex::from_u32(idx as u32);
        let sig_index = module.info.functions[func_index].entity;
        let expected = signature_id(&module.info.signatures[sig_index]);
        match lookup_import(ctx, modules, aliases, module_name, field_name)? {
            ImportType::Func(actual) if actual == expected => {}
            ImportType::Func(_) => {
                return Err(format!(
                    "incompatible function import {module_name}.{field_name}"
                ));
            }
            _ => {
                return Err(format!(
                    "incompatible import type {module_name}.{field_name}"
                ));
            }
        }
    }

    for (idx, (module_name, field_name)) in module.info.imported_globals.iter().enumerate() {
        let global_index = cranelift_wasm::GlobalIndex::from_u32(idx as u32);
        let expected = global_import_type(&module.info.globals[global_index].entity);
        match lookup_import(ctx, modules, aliases, module_name, field_name)? {
            ImportType::Global(actual) if actual == expected => {}
            ImportType::Global(_) => {
                return Err(format!(
                    "incompatible global import {module_name}.{field_name}"
                ));
            }
            _ => {
                return Err(format!(
                    "incompatible import type {module_name}.{field_name}"
                ));
            }
        }
    }

    for (idx, (module_name, field_name)) in module.info.imported_tables.iter().enumerate() {
        let table_index = cranelift_wasm::TableIndex::from_u32(idx as u32);
        let expected = TableImportType {
            ty: module.info.tables[table_index].entity.wasm_ty.to_string(),
            limits: Limits {
                min: u64::from(module.info.tables[table_index].entity.minimum),
                max: module.info.tables[table_index]
                    .entity
                    .maximum
                    .map(u64::from),
            },
        };
        match lookup_import(ctx, modules, aliases, module_name, field_name)? {
            ImportType::Table(actual) if table_import_type_matches(&expected, &actual) => {}
            ImportType::Table(_) => {
                return Err(format!(
                    "incompatible table import {module_name}.{field_name}"
                ));
            }
            _ => {
                return Err(format!(
                    "incompatible import type {module_name}.{field_name}"
                ));
            }
        }
    }

    for (idx, (module_name, field_name)) in module.info.imported_memories.iter().enumerate() {
        let memory_index = cranelift_wasm::MemoryIndex::from_u32(idx as u32);
        let memory = module.info.memories[memory_index].entity;
        let expected = Limits {
            min: memory.minimum,
            max: memory.maximum,
        };
        match lookup_import(ctx, modules, aliases, module_name, field_name)? {
            ImportType::Memory(actual) if limits_match(expected, actual) => {}
            ImportType::Memory(_) => {
                return Err(format!(
                    "incompatible memory import {module_name}.{field_name}"
                ));
            }
            _ => {
                return Err(format!(
                    "incompatible import type {module_name}.{field_name}"
                ));
            }
        }
    }

    Ok(())
}

fn active_initializer_traps(
    ctx: &mut HarnessContext, module: &NixModule, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) -> Result<bool, String> {
    for init in &module.info.data_initializers {
        let limits = memory_limits_for_index(ctx, module, modules, aliases, init.memory_index)?;
        let byte_len = limits
            .min
            .checked_mul(65536)
            .ok_or_else(|| "memory byte size overflow".to_string())?;
        let offset = active_offset(ctx, module, modules, aliases, init.base, init.offset)?;
        let end = offset.saturating_add(init.data.len() as u64);
        if offset > byte_len || end > byte_len {
            return Ok(true);
        }
    }

    for init in &module.info.table_initializers {
        let limits = table_limits_for_index(ctx, module, modules, aliases, init.table_index)?;
        let offset = active_offset(
            ctx,
            module,
            modules,
            aliases,
            init.base,
            u64::from(init.offset),
        )?;
        let end = offset.saturating_add(init.elements.len() as u64);
        if offset > limits.min || end > limits.min {
            return Ok(true);
        }
    }

    Ok(false)
}

fn active_offset(
    ctx: &mut HarnessContext, module: &NixModule, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, base: Option<cranelift_wasm::GlobalIndex>, offset: u64,
) -> Result<u64, String> {
    let Some(base) = base else {
        return Ok(offset);
    };
    Ok(global_i32_value(ctx, module, modules, aliases, base)?.saturating_add(offset))
}

fn global_i32_value(
    ctx: &mut HarnessContext, module: &NixModule, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, index: cranelift_wasm::GlobalIndex,
) -> Result<u64, String> {
    if let Some((module_name, field_name)) = module.info.imported_globals.get(index.index()) {
        return imported_global_i32_value(ctx, modules, aliases, module_name, field_name);
    }

    let Some(expr) = module.info.global_inits[index].as_ref() else {
        return Err(format!("global {index:?} has no constant initializer"));
    };
    eval_i32_const_expr(ctx, module, modules, aliases, expr)
}

fn imported_global_i32_value(
    ctx: &mut HarnessContext, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, module_name: &str, field_name: &str,
) -> Result<u64, String> {
    if module_name == "spectest" {
        return match field_name {
            "global_i32" => Ok(666),
            _ => Err(format!(
                "unsupported imported offset global {module_name}.{field_name}"
            )),
        };
    }

    let target_key = aliases
        .get(module_name)
        .cloned()
        .unwrap_or_else(|| module_name.to_string());
    let state = modules
        .get(&target_key)
        .ok_or_else(|| format!("unknown import module {module_name}"))?
        .state
        .clone();
    let actual = eval_get(
        ctx,
        &target_key,
        modules,
        aliases,
        field_name,
        state.as_deref(),
    )?;
    first_json_u64(&actual.results)
}

fn eval_i32_const_expr(
    ctx: &mut HarnessContext, module: &NixModule, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, expr: &cranelift_wasm::ConstExpr,
) -> Result<u64, String> {
    let mut stack = Vec::new();
    for op in expr.ops().iter().copied() {
        match op {
            cranelift_wasm::ConstOp::I32Const(value) => stack.push(u64::from(value as u32)),
            cranelift_wasm::ConstOp::GlobalGet(index) => {
                stack.push(global_i32_value(ctx, module, modules, aliases, index)?);
            }
            cranelift_wasm::ConstOp::I32Add => {
                let rhs = stack
                    .pop()
                    .ok_or_else(|| "malformed i32.add const expr".to_string())?;
                let lhs = stack
                    .pop()
                    .ok_or_else(|| "malformed i32.add const expr".to_string())?;
                stack.push(u64::from((lhs as u32).wrapping_add(rhs as u32)));
            }
            cranelift_wasm::ConstOp::I32Sub => {
                let rhs = stack
                    .pop()
                    .ok_or_else(|| "malformed i32.sub const expr".to_string())?;
                let lhs = stack
                    .pop()
                    .ok_or_else(|| "malformed i32.sub const expr".to_string())?;
                stack.push(u64::from((lhs as u32).wrapping_sub(rhs as u32)));
            }
            cranelift_wasm::ConstOp::I32Mul => {
                let rhs = stack
                    .pop()
                    .ok_or_else(|| "malformed i32.mul const expr".to_string())?;
                let lhs = stack
                    .pop()
                    .ok_or_else(|| "malformed i32.mul const expr".to_string())?;
                stack.push(u64::from((lhs as u32).wrapping_mul(rhs as u32)));
            }
            _ => {
                return Err(format!(
                    "unsupported active segment offset expression {op:?}"
                ));
            }
        }
    }
    if stack.len() == 1 {
        Ok(stack[0])
    } else {
        Err("malformed active segment offset expression".into())
    }
}

fn first_json_u64(value: &JsonValue) -> Result<u64, String> {
    let JsonValue::Array(items) = value else {
        return Err("expected result array".into());
    };
    let Some(first) = items.first() else {
        return Err("expected at least one result".into());
    };
    json_number_to_u64(first)
}

fn memory_limits_for_index(
    ctx: &mut HarnessContext, module: &NixModule, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, index: cranelift_wasm::MemoryIndex,
) -> Result<Limits, String> {
    if let Some((module_name, field_name)) = module.info.imported_memories.get(index.index()) {
        match lookup_import(ctx, modules, aliases, module_name, field_name)? {
            ImportType::Memory(limits) => Ok(limits),
            _ => Err(format!(
                "incompatible import type {module_name}.{field_name}"
            )),
        }
    } else {
        let memory = module.info.memories[index].entity;
        Ok(Limits {
            min: memory.minimum,
            max: memory.maximum,
        })
    }
}

fn table_limits_for_index(
    ctx: &mut HarnessContext, module: &NixModule, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, index: cranelift_wasm::TableIndex,
) -> Result<Limits, String> {
    if let Some((module_name, field_name)) = module.info.imported_tables.get(index.index()) {
        match lookup_import(ctx, modules, aliases, module_name, field_name)? {
            ImportType::Table(table) => Ok(table.limits),
            _ => Err(format!(
                "incompatible import type {module_name}.{field_name}"
            )),
        }
    } else {
        let table = module.info.tables[index].entity;
        Ok(Limits {
            min: u64::from(table.minimum),
            max: table.maximum.map(u64::from),
        })
    }
}

fn table_import_type_matches(expected: &TableImportType, actual: &TableImportType) -> bool {
    expected.ty == actual.ty && limits_match(expected.limits, actual.limits)
}

fn limits_match(expected: Limits, actual: Limits) -> bool {
    if actual.min < expected.min {
        return false;
    }
    match expected.max {
        Some(expected_max) => actual
            .max
            .is_some_and(|actual_max| actual_max <= expected_max),
        None => true,
    }
}

fn lookup_import(
    ctx: &mut HarnessContext, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, module_name: &str, field_name: &str,
) -> Result<ImportType, String> {
    if module_name == "spectest" {
        return spectest_import(field_name)
            .ok_or_else(|| format!("unknown import {module_name}.{field_name}"));
    }

    let target_key = aliases
        .get(module_name)
        .cloned()
        .unwrap_or_else(|| module_name.to_string());
    let Some(module) = modules.get(&target_key) else {
        return Err(format!("unknown import module {module_name}"));
    };

    if let Some(sig) = module.func_exports.get(field_name) {
        return Ok(ImportType::Func(sig.clone()));
    }
    if let Some(global) = module.global_export_types.get(field_name) {
        return Ok(ImportType::Global(global.clone()));
    }
    if let Some(index) = module.table_exports.get(field_name) {
        let static_type = module
            .table_export_limits
            .get(field_name)
            .ok_or_else(|| format!("missing table limits for {module_name}.{field_name}"))?
            .clone();
        let table_type = if let Some(state) = module.state.as_deref() {
            TableImportType {
                limits: dynamic_table_limits(ctx, state, *index, static_type.limits)?,
                ..static_type
            }
        } else {
            static_type
        };
        return Ok(ImportType::Table(table_type));
    }
    if let Some(index) = module.memory_exports.get(field_name) {
        let static_limits = *module
            .memory_export_limits
            .get(field_name)
            .ok_or_else(|| format!("missing memory limits for {module_name}.{field_name}"))?;
        let limits = if let Some(state) = module.state.as_deref() {
            dynamic_memory_limits(ctx, state, *index, static_limits)?
        } else {
            static_limits
        };
        return Ok(ImportType::Memory(limits));
    }

    Err(format!("unknown import {module_name}.{field_name}"))
}

fn spectest_import(field_name: &str) -> Option<ImportType> {
    match field_name {
        "print" => Some(ImportType::Func("()->()".to_string())),
        "print_i32" => Some(ImportType::Func("(i32)->()".to_string())),
        "print_i64" => Some(ImportType::Func("(i64)->()".to_string())),
        "print_f32" => Some(ImportType::Func("(f32)->()".to_string())),
        "print_f64" => Some(ImportType::Func("(f64)->()".to_string())),
        "print_i32_f32" => Some(ImportType::Func("(i32,f32)->()".to_string())),
        "print_f64_f64" => Some(ImportType::Func("(f64,f64)->()".to_string())),
        "global_i32" => Some(ImportType::Global(GlobalImportType {
            ty: "i32".to_string(),
            mutable: false,
        })),
        "global_i64" => Some(ImportType::Global(GlobalImportType {
            ty: "i64".to_string(),
            mutable: false,
        })),
        "global_f32" => Some(ImportType::Global(GlobalImportType {
            ty: "f32".to_string(),
            mutable: false,
        })),
        "global_f64" => Some(ImportType::Global(GlobalImportType {
            ty: "f64".to_string(),
            mutable: false,
        })),
        "memory" => Some(ImportType::Memory(Limits {
            min: 1,
            max: Some(2),
        })),
        "table" => Some(ImportType::Table(TableImportType {
            ty: "funcref".to_string(),
            limits: Limits {
                min: 10,
                max: Some(20),
            },
        })),
        _ => None,
    }
}

fn dynamic_memory_limits(
    ctx: &mut HarnessContext, state: &str, _index: u32, static_limits: Limits,
) -> Result<Limits, String> {
    let expr = Expr::app(
        harness_attr("dynamicMemoryLimits"),
        vec![Expr::attrset(
            false,
            vec![
                (AttrKey::ident("st"), state_expr(state)),
                (AttrKey::ident("staticMin"), nix_u64(static_limits.min)?),
                (
                    AttrKey::ident("staticMax"),
                    nix_optional_u64(static_limits.max)?,
                ),
            ],
        )],
    );
    limits_from_json(eval_nix_json(ctx, &expr)?, static_limits)
}

fn dynamic_table_limits(
    ctx: &mut HarnessContext, state: &str, index: u32, static_limits: Limits,
) -> Result<Limits, String> {
    let expr = Expr::app(
        harness_attr("dynamicTableLimits"),
        vec![Expr::attrset(
            false,
            vec![
                (AttrKey::ident("st"), state_expr(state)),
                (AttrKey::ident("tableIndex"), Expr::int(i64::from(index))),
                (AttrKey::ident("staticMin"), nix_u64(static_limits.min)?),
                (
                    AttrKey::ident("staticMax"),
                    nix_optional_u64(static_limits.max)?,
                ),
            ],
        )],
    );
    limits_from_json(eval_nix_json(ctx, &expr)?, static_limits)
}

fn limits_from_json(value: JsonValue, static_limits: Limits) -> Result<Limits, String> {
    let JsonValue::Object(map) = value else {
        return Err("limits query did not return an object".into());
    };
    let min = map
        .get("min")
        .map(json_number_to_u64)
        .transpose()?
        .unwrap_or(static_limits.min);
    let max = match map.get("max") {
        Some(JsonValue::Null) | None => static_limits.max,
        Some(value) => Some(json_number_to_u64(value)?),
    };
    Ok(Limits { min, max })
}

fn write_generated_module(ctx: &mut HarnessContext, nix: &str) -> Result<PathBuf, String> {
    ctx.write_temp_nix_file("wast", nix)
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

fn func_exports(module: &NixModule) -> HashMap<String, String> {
    module
        .info
        .functions
        .iter()
        .flat_map(|(_idx, func)| {
            let sig = signature_id(&module.info.signatures[func.entity]);
            func.export_names
                .iter()
                .cloned()
                .map(move |name| (name, sig.clone()))
        })
        .collect()
}

fn global_exports(module: &NixModule) -> HashMap<String, u32> {
    module
        .info
        .globals
        .iter()
        .flat_map(|(idx, global)| {
            global
                .export_names
                .iter()
                .cloned()
                .map(move |name| (name, idx.as_u32()))
        })
        .collect()
}

fn global_export_types(module: &NixModule) -> HashMap<String, GlobalImportType> {
    module
        .info
        .globals
        .iter()
        .flat_map(|(_idx, global)| {
            let ty = global_import_type(&global.entity);
            global
                .export_names
                .iter()
                .cloned()
                .map(move |name| (name, ty.clone()))
        })
        .collect()
}

fn memory_exports(module: &NixModule) -> HashMap<String, u32> {
    module
        .info
        .memories
        .iter()
        .flat_map(|(idx, memory)| {
            memory
                .export_names
                .iter()
                .cloned()
                .map(move |name| (name, idx.as_u32()))
        })
        .collect()
}

fn memory_export_limits(module: &NixModule) -> HashMap<String, Limits> {
    module
        .info
        .memories
        .iter()
        .flat_map(|(_idx, memory)| {
            let limits = Limits {
                min: memory.entity.minimum,
                max: memory.entity.maximum,
            };
            memory
                .export_names
                .iter()
                .cloned()
                .map(move |name| (name, limits))
        })
        .collect()
}

fn table_export_limits(module: &NixModule) -> HashMap<String, TableImportType> {
    module
        .info
        .tables
        .iter()
        .flat_map(|(_idx, table)| {
            let table_type = TableImportType {
                ty: table.entity.wasm_ty.to_string(),
                limits: Limits {
                    min: u64::from(table.entity.minimum),
                    max: table.entity.maximum.map(u64::from),
                },
            };
            table
                .export_names
                .iter()
                .cloned()
                .map(move |name| (name, table_type.clone()))
        })
        .collect()
}

fn signature_id(sig: &ir::Signature) -> String {
    let params = sig
        .params
        .iter()
        .map(|param| param.value_type.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let returns = sig
        .returns
        .iter()
        .map(|param| param.value_type.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!("({params})->({returns})")
}

fn global_import_type(global: &cranelift_wasm::Global) -> GlobalImportType {
    GlobalImportType {
        ty: global.wasm_ty.to_string(),
        mutable: global.mutability,
    }
}

struct InvokeResult {
    results: JsonValue,
    state: String,
}

fn eval_instantiate(
    ctx: &mut HarnessContext, module: &Path, module_key: &str,
    modules: &HashMap<String, HarnessModule>, aliases: &HashMap<String, String>,
) -> Result<String, String> {
    let world = EvalWorld::new(modules, aliases);
    let imports = imports_expr(module_key, aliases, &world.module_vars);
    let expr = Expr::let_in(
        eval_binds(
            world,
            vec![("m_new".into(), import_module_expr(module, imports))],
        )?,
        Expr::let_in(
            vec![(
                "i".into(),
                Expr::app(
                    Expr::select(Expr::ident("m_new"), AttrKey::ident("instantiate")),
                    vec![empty_attrset()],
                ),
            )],
            Expr::select(Expr::ident("i"), AttrKey::ident("st")),
        ),
    );
    let value = eval_nix_json(ctx, &expr)?;
    json_to_nix_expr(&value)
}

fn eval_initial_state(
    ctx: &mut HarnessContext, module: &Path, module_key: &str,
    modules: &HashMap<String, HarnessModule>, aliases: &HashMap<String, String>,
) -> Result<String, String> {
    let world = EvalWorld::new(modules, aliases);
    let imports = imports_expr(module_key, aliases, &world.module_vars);
    let expr = Expr::let_in(
        eval_binds(
            world,
            vec![("m_new".into(), import_module_expr(module, imports))],
        )?,
        select_string_path(Expr::ident("m_new"), &["__wasm2nix", "st0"]),
    );
    let value = eval_nix_json(ctx, &expr)?;
    json_to_nix_expr(&value)
}

fn eval_invoke(
    ctx: &mut HarnessContext, module_key: &str, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, name: &str, args: &[WastArg<'_>], state: Option<&str>,
) -> Result<InvokeResult, String> {
    let args = args
        .iter()
        .map(wast_arg_to_nix)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .chain(std::iter::once(Expr::ident("st_in")))
        .collect::<Vec<_>>();
    let state_expr = state
        .map(state_expr)
        .unwrap_or_else(|| Expr::select(Expr::ident("i"), AttrKey::ident("st")));
    let world = EvalWorld::new(modules, aliases);
    let module_var = world
        .module_vars
        .get(module_key)
        .ok_or_else(|| format!("unknown module {module_key}"))?
        .clone();
    let export = Expr::select(
        select_string_path(
            Expr::ident(module_var.as_str()),
            &["__wasm2nix", "exportsByName"],
        ),
        AttrKey::string(encoded_export_key(name)),
    );
    let expr = Expr::let_in(
        eval_binds(
            world,
            vec![
                (
                    "i".into(),
                    Expr::app(
                        Expr::select(Expr::ident(module_var), AttrKey::ident("instantiate")),
                        vec![empty_attrset()],
                    ),
                ),
                ("st_in".into(), state_expr),
                ("call".into(), Expr::app(export, args)),
            ],
        )?,
        Expr::attrset(
            false,
            vec![
                (
                    AttrKey::ident("results"),
                    Expr::select(Expr::ident("call"), AttrKey::ident("results")),
                ),
                (
                    AttrKey::ident("st"),
                    Expr::select(Expr::ident("call"), AttrKey::ident("st")),
                ),
            ],
        ),
    );
    let value = eval_nix_json(ctx, &expr)?;
    let results = value["results"].clone();
    let state = json_to_nix_expr(&value["st"])?;
    Ok(InvokeResult { results, state })
}

fn eval_get(
    ctx: &mut HarnessContext, module_key: &str, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, name: &str, state: Option<&str>,
) -> Result<InvokeResult, String> {
    let state_expr = state
        .map(state_expr)
        .unwrap_or_else(|| Expr::select(Expr::ident("i"), AttrKey::ident("st")));
    let world = EvalWorld::new(modules, aliases);
    let module_var = world
        .module_vars
        .get(module_key)
        .ok_or_else(|| format!("unknown module {module_key}"))?
        .clone();
    let export = Expr::select(
        select_string_path(
            Expr::ident(module_var.as_str()),
            &["__wasm2nix", "exportsByName"],
        ),
        AttrKey::string(encoded_export_key(name)),
    );
    let expr = Expr::let_in(
        eval_binds(
            world,
            vec![
                (
                    "i".into(),
                    Expr::app(
                        Expr::select(Expr::ident(module_var), AttrKey::ident("instantiate")),
                        vec![empty_attrset()],
                    ),
                ),
                ("st_in".into(), state_expr),
                (
                    "value".into(),
                    Expr::app(
                        Expr::select(export, AttrKey::ident("get")),
                        vec![Expr::ident("st_in")],
                    ),
                ),
            ],
        )?,
        Expr::attrset(
            false,
            vec![
                (
                    AttrKey::ident("results"),
                    Expr::list(vec![Expr::ident("value")]),
                ),
                (AttrKey::ident("st"), Expr::ident("st_in")),
            ],
        ),
    );
    let value = eval_nix_json(ctx, &expr)?;
    let results = value["results"].clone();
    let state = json_to_nix_expr(&value["st"])?;
    Ok(InvokeResult { results, state })
}

fn eval_nix_json(ctx: &mut HarnessContext, expr: &Expr) -> Result<JsonValue, String> {
    let expr = Expr::let_in(
        vec![("harness".into(), harness_import_expr()?)],
        Expr::app(harness_attr("toJSON"), vec![expr.clone()]),
    );
    let rendered = expr.render();
    let path = ctx.write_temp_nix_file("eval", &rendered)?;
    let output = Command::new("nix")
        .args(["eval", "--raw", "--impure", "-f"])
        .arg(&path)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    Ok(value)
}

fn rt_import_expr() -> Result<Expr, String> {
    let path = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join("nix/rt.nix");
    Ok(import_nix_file_expr(&path))
}

fn harness_import_expr() -> Result<Expr, String> {
    let path = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join("nix/harness.nix");
    Ok(import_nix_file_expr(&path))
}

fn eval_binds(world: EvalWorld, extra: Vec<(String, Expr)>) -> Result<Vec<(String, Expr)>, String> {
    let mut binds = Vec::with_capacity(world.binds.len() + extra.len() + 1);
    binds.push(("rt".into(), rt_import_expr()?));
    binds.extend(world.binds);
    binds.extend(extra);
    Ok(binds)
}

fn import_nix_file_expr(path: &Path) -> Expr {
    Expr::app(
        Expr::ident("import"),
        vec![
            Expr::path(path.display().to_string()),
            Expr::attrset(false, vec![]),
        ],
    )
}

fn import_module_expr(path: &Path, imports: Expr) -> Expr {
    Expr::app(
        Expr::ident("import"),
        vec![
            Expr::path(path.display().to_string()),
            Expr::ident("rt"),
            imports,
        ],
    )
}

fn empty_attrset() -> Expr {
    Expr::attrset(false, vec![])
}

fn harness_attr(name: &str) -> Expr {
    Expr::select(Expr::ident("harness"), AttrKey::ident(name))
}

fn select_string_path(mut expr: Expr, attrs: &[&str]) -> Expr {
    for attr in attrs {
        expr = Expr::select(expr, AttrKey::string(*attr));
    }
    expr
}

fn state_expr(state: &str) -> Expr {
    Expr::raw(state.to_string())
}

fn nix_u64(value: u64) -> Result<Expr, String> {
    i64::try_from(value)
        .map(Expr::int)
        .map_err(|_| format!("Nix integer literal overflow for {value}"))
}

fn nix_optional_u64(value: Option<u64>) -> Result<Expr, String> {
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
            let module_state_expr = module.state.as_deref().map(state_expr).unwrap_or_else(|| {
                Expr::select(
                    Expr::app(
                        Expr::select(Expr::ident(raw_var.as_str()), AttrKey::ident("instantiate")),
                        vec![empty_attrset()],
                    ),
                    AttrKey::ident("st"),
                )
            });
            binds.push((state_var.clone(), module_state_expr));
            binds.push((
                import_var.clone(),
                Expr::bin(
                    BinOp::Update,
                    Expr::ident(raw_var.as_str()),
                    Expr::attrset(
                        false,
                        vec![(
                            AttrKey::string("__wasm2nixState"),
                            Expr::ident(state_var.as_str()),
                        )],
                    ),
                ),
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

fn apply_trapping_module_side_effects(
    ctx: &mut HarnessContext, directive_index: usize, wat: &mut wast::Wat<'_>,
    modules: &mut HashMap<String, HarnessModule>, aliases: &HashMap<String, String>,
) -> Result<(), String> {
    let wasm = wat
        .encode()
        .map_err(|e| format!("directive {directive_index}: {e}"))?;
    let translated =
        NixModule::translate(&wasm).map_err(|e| format!("directive {directive_index}: {e}"))?;
    validate_imports_for_linking(ctx, &translated, modules, aliases)
        .map_err(|e| format!("directive {directive_index}: {e}"))?;
    let initializer_traps = active_initializer_traps(ctx, &translated, modules, aliases)
        .map_err(|e| format!("directive {directive_index}: {e}"))?;
    let nix = wasm2nix::clif_to_nix::lower_module(&translated).render();
    let path = write_generated_module(ctx, &nix)?;
    let transient_key = format!("__trap{directive_index}");
    let source_state = eval_initial_state(ctx, &path, &transient_key, modules, aliases)
        .map_err(|e| format!("directive {directive_index}: {e}"))?;
    if !initializer_traps && eval_instantiate(ctx, &path, &transient_key, modules, aliases).is_ok()
    {
        return Err(format!(
            "directive {directive_index}: assert_trap module instantiated successfully"
        ));
    }
    modules.insert(
        transient_key,
        HarnessModule {
            path,
            state: Some(source_state.clone()),
            imported_tables: translated.info.imported_tables.clone(),
            imported_globals: translated.info.imported_globals.clone(),
            imported_memories: translated.info.imported_memories.clone(),
            func_exports: func_exports(&translated),
            table_exports: table_exports(&translated),
            global_exports: global_exports(&translated),
            memory_exports: memory_exports(&translated),
            table_export_limits: table_export_limits(&translated),
            global_export_types: global_export_types(&translated),
            memory_export_limits: memory_export_limits(&translated),
        },
    );
    apply_trapping_table_side_effects(&translated, &source_state, modules, aliases);
    apply_trapping_memory_side_effects(&translated, &source_state, modules, aliases);
    Ok(())
}

fn apply_trapping_table_side_effects(
    module: &NixModule, source_state: &str, modules: &mut HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) {
    for init in &module.info.table_initializers {
        if init.base.is_some() {
            continue;
        }
        let Some((import_module, import_field)) =
            module.info.imported_tables.get(init.table_index.index())
        else {
            continue;
        };
        let table_size = module.info.tables[init.table_index].entity.minimum;
        let end = init.offset.saturating_add(init.elements.len() as u32);
        if end > table_size {
            break;
        }

        let target_key = aliases
            .get(import_module)
            .cloned()
            .unwrap_or_else(|| import_module.clone());
        let Some(target_module) = modules.get(&target_key) else {
            continue;
        };
        let Some(target_table_index) = target_module.table_exports.get(import_field).copied()
        else {
            continue;
        };
        let Some(target_state) = target_module.state.clone() else {
            continue;
        };
        let merged = merge_table_entries_expr(
            &target_state,
            target_table_index,
            source_state,
            init.table_index.as_u32(),
            init.offset,
            init.elements.len() as u32,
        );
        if let Some(module) = modules.get_mut(&target_key) {
            module.state = Some(merged);
        }
    }
}

fn merge_table_entries_expr(
    target_state: &str, target_table_index: u32, source_state: &str, source_table_index: u32,
    offset: u32, len: u32,
) -> String {
    Expr::app(
        harness_attr("mergeTableEntries"),
        vec![Expr::attrset(
            false,
            vec![
                (AttrKey::ident("dstSt"), state_expr(target_state)),
                (
                    AttrKey::ident("targetTableIndex"),
                    Expr::int(i64::from(target_table_index)),
                ),
                (AttrKey::ident("srcSt"), state_expr(source_state)),
                (
                    AttrKey::ident("sourceTableIndex"),
                    Expr::int(i64::from(source_table_index)),
                ),
                (AttrKey::ident("offset"), Expr::int(i64::from(offset))),
                (AttrKey::ident("len"), Expr::int(i64::from(len))),
            ],
        )],
    )
    .render()
}

fn apply_trapping_memory_side_effects(
    module: &NixModule, source_state: &str, modules: &mut HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) {
    for init in &module.info.data_initializers {
        if init.base.is_some() {
            continue;
        }
        let Some((import_module, import_field)) =
            module.info.imported_memories.get(init.memory_index.index())
        else {
            continue;
        };
        let memory_min_pages = module.info.memories[init.memory_index].entity.minimum;
        let end = init.offset.saturating_add(init.data.len() as u64);
        if end > memory_min_pages.saturating_mul(65536) {
            break;
        }

        let target_key = aliases
            .get(import_module)
            .cloned()
            .unwrap_or_else(|| import_module.clone());
        let Some(target_module) = modules.get(&target_key) else {
            continue;
        };
        let Some(_target_memory_index) = target_module.memory_exports.get(import_field).copied()
        else {
            continue;
        };
        let Some(target_state) = target_module.state.clone() else {
            continue;
        };
        let merged =
            merge_memory_words_expr(&target_state, source_state, init.offset, init.data.len());
        if let Some(module) = modules.get_mut(&target_key) {
            module.state = Some(merged);
        }
    }
}

fn merge_memory_words_expr(
    target_state: &str, source_state: &str, offset: u64, len: usize,
) -> String {
    Expr::app(
        harness_attr("mergeMemoryWords"),
        vec![Expr::attrset(
            false,
            vec![
                (AttrKey::ident("dstSt"), state_expr(target_state)),
                (AttrKey::ident("srcSt"), state_expr(source_state)),
                (
                    AttrKey::ident("offset"),
                    nix_u64(offset).unwrap_or_else(|_| Expr::raw(offset.to_string())),
                ),
                (
                    AttrKey::ident("len"),
                    nix_u64(len as u64).unwrap_or_else(|_| Expr::raw(len.to_string())),
                ),
            ],
        )],
    )
    .render()
}

fn sync_global_imports_from_targets(
    modules: &mut HashMap<String, HarnessModule>, aliases: &HashMap<String, String>,
) {
    let mut keys = modules.keys().cloned().collect::<Vec<_>>();
    keys.sort();

    for source_key in keys {
        let Some(source_module) = modules.get(&source_key) else {
            continue;
        };
        let mut source_state = match source_module.state.clone() {
            Some(state) => state,
            None => continue,
        };
        let imports = source_module.imported_globals.clone();

        for (source_global_index, (import_module, import_field)) in imports.iter().enumerate() {
            let target_key = aliases
                .get(import_module)
                .cloned()
                .unwrap_or_else(|| import_module.clone());
            let Some(target_module) = modules.get(&target_key) else {
                continue;
            };
            let Some(target_global_index) = target_module.global_exports.get(import_field).copied()
            else {
                continue;
            };
            let Some(target_state) = target_module.state.clone() else {
                continue;
            };
            source_state = sync_global_state_expr(
                &source_state,
                source_global_index as u32,
                &target_state,
                target_global_index,
            );
        }

        if let Some(module) = modules.get_mut(&source_key) {
            module.state = Some(source_state);
        }
    }
}

fn sync_global_imports_to_targets(
    source_key: &str, modules: &mut HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) {
    let Some(source_module) = modules.get(source_key) else {
        return;
    };
    let Some(source_state) = source_module.state.clone() else {
        return;
    };
    let imports = source_module.imported_globals.clone();

    for (source_global_index, (import_module, import_field)) in imports.iter().enumerate() {
        let target_key = aliases
            .get(import_module)
            .cloned()
            .unwrap_or_else(|| import_module.clone());
        let Some(target_module) = modules.get(&target_key) else {
            continue;
        };
        let Some(target_global_index) = target_module.global_exports.get(import_field).copied()
        else {
            continue;
        };
        let Some(target_state) = target_module.state.clone() else {
            continue;
        };
        let synced = sync_global_state_expr(
            &target_state,
            target_global_index,
            &source_state,
            source_global_index as u32,
        );
        if let Some(module) = modules.get_mut(&target_key) {
            module.state = Some(synced);
        }
    }
}

fn sync_global_state_expr(
    dst_state: &str, dst_global_index: u32, src_state: &str, src_global_index: u32,
) -> String {
    Expr::app(
        harness_attr("syncGlobalState"),
        vec![Expr::attrset(
            false,
            vec![
                (AttrKey::ident("dstSt"), state_expr(dst_state)),
                (
                    AttrKey::ident("dstGlobalIndex"),
                    Expr::int(i64::from(dst_global_index)),
                ),
                (AttrKey::ident("srcSt"), state_expr(src_state)),
                (
                    AttrKey::ident("srcGlobalIndex"),
                    Expr::int(i64::from(src_global_index)),
                ),
            ],
        )],
    )
    .render()
}

fn sync_memory_imports_to_targets(
    source_key: &str, modules: &mut HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) {
    let Some(source_module) = modules.get(source_key) else {
        return;
    };
    let Some(source_state) = source_module.state.clone() else {
        return;
    };
    let imports = source_module.imported_memories.clone();

    for (source_memory_index, (import_module, import_field)) in imports.iter().enumerate() {
        let target_key = aliases
            .get(import_module)
            .cloned()
            .unwrap_or_else(|| import_module.clone());
        let Some(target_module) = modules.get(&target_key) else {
            continue;
        };
        let Some(_target_memory_index) = target_module.memory_exports.get(import_field).copied()
        else {
            continue;
        };
        let Some(target_state) = target_module.state.clone() else {
            continue;
        };
        let synced =
            sync_memory_state_expr(&target_state, &source_state, source_memory_index as u32);
        if let Some(module) = modules.get_mut(&target_key) {
            module.state = Some(synced);
        }
    }
}

fn sync_memory_state_expr(dst_state: &str, src_state: &str, _src_memory_index: u32) -> String {
    Expr::app(
        harness_attr("syncMemoryState"),
        vec![Expr::attrset(
            false,
            vec![
                (AttrKey::ident("dstSt"), state_expr(dst_state)),
                (AttrKey::ident("srcSt"), state_expr(src_state)),
                (
                    AttrKey::ident("srcMemoryIndex"),
                    Expr::int(i64::from(_src_memory_index)),
                ),
            ],
        )],
    )
    .render()
}

fn merge_imported_table_initializers(
    module_key: &str, modules: &mut HashMap<String, HarnessModule>,
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
    target_state: &str, target_table_index: u32, source_state: &str, source_table_index: u32,
) -> String {
    Expr::app(
        harness_attr("mergeTableState"),
        vec![Expr::attrset(
            false,
            vec![
                (AttrKey::ident("dstSt"), state_expr(target_state)),
                (
                    AttrKey::ident("targetTableIndex"),
                    Expr::int(i64::from(target_table_index)),
                ),
                (AttrKey::ident("srcSt"), state_expr(source_state)),
                (
                    AttrKey::ident("sourceTableIndex"),
                    Expr::int(i64::from(source_table_index)),
                ),
            ],
        )],
    )
    .render()
}

fn encoded_export_key(name: &str) -> String {
    let mut encoded = String::from("__wasm2nix_export_");
    for byte in name.as_bytes() {
        use std::fmt::Write;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn wast_arg_to_nix(arg: &WastArg<'_>) -> Result<Expr, String> {
    match arg {
        WastArg::Core(WastArgCore::I32(value)) => Ok(Expr::int(i64::from(*value as u32))),
        WastArg::Core(WastArgCore::I64(value)) => Ok(Expr::int(*value)),
        WastArg::Core(WastArgCore::F32(value)) => Ok(Expr::int(i64::from(value.bits))),
        WastArg::Core(WastArgCore::F64(value)) => Ok(Expr::int(value.bits as i64)),
        WastArg::Core(WastArgCore::RefExtern(value)) => Ok(Expr::attrset(
            false,
            vec![(AttrKey::ident("extern"), Expr::int(i64::from(*value)))],
        )),
        WastArg::Core(WastArgCore::RefNull(_)) => Ok(Expr::int(0)),
        _ => Err("unsupported invoke argument for this harness slice".into()),
    }
}

fn json_to_nix_expr(value: &JsonValue) -> Result<String, String> {
    let json = serde_json::to_string(value).map_err(|e| e.to_string())?;
    Ok(Expr::app(harness_attr("fromJSON"), vec![Expr::string(json)]).render())
}

fn expected_results_json(results: &[WastRet<'_>]) -> Result<String, String> {
    let values = results
        .iter()
        .map(expected_result_label)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!("[{}]", values.join(",")))
}

fn expected_result_label(ret: &WastRet<'_>) -> Result<String, String> {
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
        WastRet::Core(WastRetCore::RefExtern(Some(value))) => Ok(format!("{{\"extern\":{value}}}")),
        WastRet::Core(WastRetCore::RefNull(_)) => Ok("0".to_string()),
        _ => Err("unsupported assert_return result for this harness slice".into()),
    }
}

fn expected_result_value(ret: &WastRet<'_>) -> Result<JsonValue, String> {
    match ret {
        WastRet::Core(WastRetCore::I32(value)) => Ok(JsonValue::from(*value as u32)),
        WastRet::Core(WastRetCore::I64(value)) => Ok(JsonValue::from(*value)),
        WastRet::Core(WastRetCore::F32(NanPattern::Value(value))) => {
            Ok(JsonValue::from(value.bits))
        }
        WastRet::Core(WastRetCore::F64(NanPattern::Value(value))) => {
            Ok(JsonValue::from(value.bits as i64))
        }
        WastRet::Core(WastRetCore::RefExtern(Some(value))) => {
            Ok(serde_json::json!({ "extern": value }))
        }
        WastRet::Core(WastRetCore::RefNull(_)) => Ok(JsonValue::from(0)),
        WastRet::Core(WastRetCore::F32(NanPattern::CanonicalNan))
        | WastRet::Core(WastRetCore::F64(NanPattern::CanonicalNan))
        | WastRet::Core(WastRetCore::F32(NanPattern::ArithmeticNan))
        | WastRet::Core(WastRetCore::F64(NanPattern::ArithmeticNan)) => {
            Err("NaN patterns must be matched by predicate".into())
        }
        _ => Err("unsupported assert_return result for this harness slice".into()),
    }
}

fn results_match(expected: &[WastRet<'_>], actual: &JsonValue) -> Result<bool, String> {
    let JsonValue::Array(actual_items) = actual else {
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
        _ => Ok(*actual == expected_result_value(expected)?),
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
    (bits & 0x7F80_0000) == 0x7F80_0000 && (bits & 0x007F_FFFF) == 0x0040_0000
}

fn f32_arithmetic_nan(bits: u32) -> bool {
    (bits & 0x7F80_0000) == 0x7F80_0000 && (bits & 0x0040_0000) != 0
}

fn f64_canonical_nan(bits: u64) -> bool {
    (bits & 0x7FF0_0000_0000_0000) == 0x7FF0_0000_0000_0000
        && (bits & 0x000F_FFFF_FFFF_FFFF) == 0x0008_0000_0000_0000
}

fn f64_arithmetic_nan(bits: u64) -> bool {
    (bits & 0x7FF0_0000_0000_0000) == 0x7FF0_0000_0000_0000 && (bits & 0x0008_0000_0000_0000) != 0
}

include!(concat!(env!("OUT_DIR"), "/wast_generated_tests.rs"));
