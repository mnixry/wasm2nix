use std::{collections::HashMap, path::PathBuf};

use cranelift_entity::{EntityRef, PrimaryMap};
use serde_json::Value as JsonValue;
use snafu::prelude::*;
use tempfile::{Builder as TempFileBuilder, TempDir};
use wasm2nix::{
    instantiation::ActiveInitializerResolver,
    linking::{
        self, ExternType, FuncType, GlobalType, ImportResolver, Limits, ModuleInterface, TableType,
    },
    nix::nix,
    translator::{Exportable, NixModule},
};
use wast::{
    Wast, WastDirective, WastExecute,
    lexer::Lexer,
    parser::{self, ParseBuffer},
};

use crate::{
    error::{
        self, ActiveInitializerSnafu, CreateTempDirSnafu, CreateTempFileSnafu, EncodeWatSnafu,
        Error, ExpectedAtLeastOneResultSnafu, ExpectedResultArraySnafu, HarnessResolverError,
        IncompatibleImportTypeSnafu, KeepTempFileSnafu, LimitsQueryNotObjectSnafu, LinkSnafu,
        LowerNixSnafu, MissingTableLimitsSnafu, ModuleAccessBeforeModuleSnafu, ParseWastSnafu,
        Result, TranslateWasmSnafu, UnknownImportModuleSnafu, UnknownImportSnafu,
        UnknownModuleSnafu, WriteTempFileSnafu,
    },
    eval::{
        eval_get, eval_instantiate, eval_invoke, eval_nix_json, nix_optional_u64, nix_u64,
        state_expr,
    },
    state::{
        apply_trapping_module_side_effects, expected_results_json, json_number_to_u64,
        merge_imported_table_initializers, results_match, sync_global_imports_from_targets,
        sync_global_imports_to_targets, sync_memory_imports_to_targets,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WastHarnessOptions {
    pub trace_prints: bool,
}

impl Default for WastHarnessOptions {
    fn default() -> Self {
        Self { trace_prints: true }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WastHarness {
    options: WastHarnessOptions,
}

impl WastHarness {
    #[must_use]
    pub const fn new(options: WastHarnessOptions) -> Self {
        Self { options }
    }

    /// Runs a WAST script.
    ///
    /// # Errors
    ///
    /// Returns an error when the script cannot be parsed, translated, linked,
    /// evaluated, or when an assertion fails.
    pub fn run_wast_script(&self, script: &str) -> Result<()> {
        run_wast_script_with_options(script, self.options)
    }
}

pub struct HarnessContext {
    pub temp_dir: TempDir,
    pub options: WastHarnessOptions,
}

impl HarnessContext {
    fn new(options: WastHarnessOptions) -> Result<Self> {
        Ok(Self {
            temp_dir: TempDir::new().context(CreateTempDirSnafu)?,
            options,
        })
    }

    pub fn write_temp_nix_file(&self, kind: &str, nix: &str) -> Result<PathBuf> {
        use std::io::Write;

        let mut file = TempFileBuilder::new()
            .prefix(&format!("{kind}-"))
            .suffix(".nix")
            .tempfile_in(self.temp_dir.path())
            .context(CreateTempFileSnafu {
                kind: kind.to_string(),
            })?;
        file.write_all(nix.as_bytes()).context(WriteTempFileSnafu {
            kind: kind.to_string(),
        })?;
        let (_file, path) = file.keep().context(KeepTempFileSnafu {
            kind: kind.to_string(),
        })?;
        Ok(path)
    }
}

/// Runs a WAST script with default harness options.
///
/// # Errors
///
/// Returns an error when the script cannot be parsed, translated, linked,
/// evaluated, or when an assertion fails.
pub fn run_wast_script(script: &str) -> Result<()> {
    WastHarness::default().run_wast_script(script)
}

#[allow(clippy::too_many_lines)]
fn run_wast_script_with_options(script: &str, options: WastHarnessOptions) -> Result<()> {
    let ctx = HarnessContext::new(options)?;
    let mut lexer = Lexer::new(script);
    lexer.allow_confusing_unicode(true);
    let buf = ParseBuffer::new_with_lexer(lexer).context(ParseWastSnafu)?;
    let directives = parser::parse::<Wast<'_>>(&buf)
        .context(ParseWastSnafu)?
        .directives;
    let mut modules: HashMap<String, HarnessModule> = HashMap::new();
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut current_module: Option<String> = None;
    let mut next_anon_module = 0usize;

    for (directive_index, directive) in directives.into_iter().enumerate() {
        match directive {
            WastDirective::Module(mut module) => {
                let module_name = module.name().map(|id| id.name().to_string());
                let wasm =
                    error::directive(directive_index, module.encode().context(EncodeWatSnafu))?;
                let translated = error::directive(
                    directive_index,
                    NixModule::translate(&wasm).context(TranslateWasmSnafu),
                )?;
                error::directive(
                    directive_index,
                    validate_imports_for_linking(&ctx, &translated, &modules, &aliases),
                )?;
                if active_initializer_traps(&ctx, &translated, &modules, &aliases)? {
                    return Err(Error::Directive {
                        directive_index,
                        source: Box::new(Error::ModuleInstantiationTrapped),
                    });
                }
                let nix = error::directive(
                    directive_index,
                    wasm2nix::lower::lower_module(&translated).context(LowerNixSnafu),
                )?
                .render();
                let path = write_generated_module(&ctx, &nix)?;
                let key = module_name.unwrap_or_else(|| {
                    let key = format!("__anon{next_anon_module}");
                    next_anon_module += 1;
                    key
                });
                let state = eval_instantiate(&ctx, &path, &key, &modules, &aliases)?;
                modules.insert(
                    key.clone(),
                    HarnessModule::new(path, Some(state), &translated),
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
                            .context(UnknownModuleSnafu { key: key.clone() })?
                            .state
                            .clone();
                        let actual = error::invoke(
                            directive_index,
                            invoke.name,
                            eval_invoke(
                                &ctx,
                                &key,
                                &modules,
                                &aliases,
                                invoke.name,
                                &invoke.args,
                                state.as_deref(),
                            ),
                        )?;
                        modules
                            .get_mut(&key)
                            .context(UnknownModuleSnafu { key: key.clone() })?
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
                            .context(UnknownModuleSnafu { key: key.clone() })?
                            .state
                            .clone();
                        error::directive(
                            directive_index,
                            eval_get(&ctx, &key, &modules, &aliases, global, state.as_deref()),
                        )?
                    }
                    WastExecute::Wat(_) => {
                        return Err(Error::Directive {
                            directive_index,
                            source: Box::new(Error::UnsupportedAssertReturnModuleExecution),
                        });
                    }
                };
                let expected = expected_results_json(&results)?;
                if !results_match(&results, &actual.results)? {
                    return Err(Error::Directive {
                        directive_index,
                        source: Box::new(Error::AssertReturnMismatch {
                            expected,
                            actual: actual.results,
                        }),
                    });
                }
            }
            WastDirective::AssertTrap { exec, .. } => match exec {
                WastExecute::Invoke(invoke) => {
                    let key =
                        select_module_key(invoke.module, current_module.as_deref(), &aliases)?;
                    let module = modules
                        .get(&key)
                        .context(UnknownModuleSnafu { key: key.clone() })?;
                    if eval_invoke(
                        &ctx,
                        &key,
                        &modules,
                        &aliases,
                        invoke.name,
                        &invoke.args,
                        module.state.as_deref(),
                    )
                    .is_ok()
                    {
                        return Err(Error::InvokeDidNotTrap {
                            name: invoke.name.to_string(),
                        });
                    }
                }
                WastExecute::Get { module, global, .. } => {
                    let key = select_module_key(module, current_module.as_deref(), &aliases)?;
                    let module = modules
                        .get(&key)
                        .context(UnknownModuleSnafu { key: key.clone() })?;
                    if eval_get(
                        &ctx,
                        &key,
                        &modules,
                        &aliases,
                        global,
                        module.state.as_deref(),
                    )
                    .is_ok()
                    {
                        return Err(Error::GetDidNotTrap {
                            global: global.to_string(),
                        });
                    }
                }
                WastExecute::Wat(mut inline_module) => {
                    apply_trapping_module_side_effects(
                        &ctx,
                        directive_index,
                        &mut inline_module,
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
                if wasmparser::Validator::new_with_features(wasm2nix::translator::WASM_FEATURES)
                    .validate_all(&wasm)
                    .is_err()
                {
                    continue;
                }
                if NixModule::translate(&wasm).is_err() {
                    continue;
                }
                return Err(Error::Directive {
                    directive_index,
                    source: Box::new(Error::ModuleAccepted {
                        assertion: "assert_malformed",
                    }),
                });
            }
            WastDirective::AssertInvalid { mut module, .. }
            | WastDirective::AssertInvalidCustom { mut module, .. } => {
                let Ok(wasm) = module.encode() else {
                    continue;
                };
                if NixModule::translate(&wasm).is_ok() {
                    return Err(Error::Directive {
                        directive_index,
                        source: Box::new(Error::InvalidModuleTranslated),
                    });
                }
            }
            WastDirective::AssertUnlinkable { mut module, .. } => {
                let wasm =
                    error::directive(directive_index, module.encode().context(EncodeWatSnafu))?;
                let translated = error::directive(
                    directive_index,
                    NixModule::translate(&wasm).context(TranslateWasmSnafu),
                )?;
                if validate_imports_for_linking(&ctx, &translated, &modules, &aliases).is_err() {
                    continue;
                }
                let nix = error::directive(
                    directive_index,
                    wasm2nix::lower::lower_module(&translated).context(LowerNixSnafu),
                )?
                .render();
                let path = write_generated_module(&ctx, &nix)?;
                let transient_key = format!("__unlinkable{directive_index}");
                if eval_instantiate(&ctx, &path, &transient_key, &modules, &aliases).is_ok() {
                    return Err(Error::Directive {
                        directive_index,
                        source: Box::new(Error::UnlinkableModuleInstantiated),
                    });
                }
            }
            WastDirective::AssertExhaustion { call, .. } => {
                let key = select_module_key(call.module, current_module.as_deref(), &aliases)?;
                let module = modules
                    .get(&key)
                    .context(UnknownModuleSnafu { key: key.clone() })?;
                if eval_invoke(
                    &ctx,
                    &key,
                    &modules,
                    &aliases,
                    call.name,
                    &call.args,
                    module.state.as_deref(),
                )
                .is_ok()
                {
                    return Err(Error::Directive {
                        directive_index,
                        source: Box::new(Error::ExhaustionDidNotFail {
                            name: call.name.to_string(),
                        }),
                    });
                }
            }
            WastDirective::Invoke(invoke) => {
                let key = select_module_key(invoke.module, current_module.as_deref(), &aliases)?;
                let state = modules
                    .get(&key)
                    .context(UnknownModuleSnafu { key: key.clone() })?
                    .state
                    .clone();
                let actual = eval_invoke(
                    &ctx,
                    &key,
                    &modules,
                    &aliases,
                    invoke.name,
                    &invoke.args,
                    state.as_deref(),
                )?;
                modules
                    .get_mut(&key)
                    .context(UnknownModuleSnafu { key: key.clone() })?
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
                return Err(Error::Directive {
                    directive_index,
                    source: Box::new(Error::UnsupportedWastDirective),
                });
            }
        }
    }

    Ok(())
}

pub struct HarnessModule {
    pub path: PathBuf,
    pub state: Option<String>,
    pub imported_tables: Vec<(String, String)>,
    pub imported_globals: Vec<(String, String)>,
    pub imported_memories: Vec<(String, String)>,
    /// Export types by name.
    pub interface: ModuleInterface,
    /// Export indices by name, used to address the exporting module's state.
    pub table_exports: HashMap<String, u32>,
    pub global_exports: HashMap<String, u32>,
    pub memory_exports: HashMap<String, u32>,
}

impl HarnessModule {
    pub fn new(path: PathBuf, state: Option<String>, module: &NixModule) -> Self {
        let info = &module.info;
        Self {
            path,
            state,
            imported_tables: info.imported_tables.clone(),
            imported_globals: info.imported_globals.clone(),
            imported_memories: info.imported_memories.clone(),
            interface: ModuleInterface::from_module(module),
            table_exports: export_indices(&info.tables),
            global_exports: export_indices(&info.globals),
            memory_exports: export_indices(&info.memories),
        }
    }
}

fn export_indices<K: EntityRef, E>(
    entities: &PrimaryMap<K, Exportable<E>>,
) -> HashMap<String, u32> {
    entities
        .iter()
        .flat_map(|(index, entity)| {
            let index = u32::try_from(index.index()).expect("Wasm indices fit in u32");
            entity
                .export_names
                .iter()
                .map(move |name| (name.clone(), index))
        })
        .collect()
}

fn select_module_key(
    module: Option<wast::token::Id<'_>>, current: Option<&str>, aliases: &HashMap<String, String>,
) -> Result<String> {
    module.map_or_else(
        || {
            current
                .map(str::to_string)
                .context(ModuleAccessBeforeModuleSnafu)
        },
        |module| {
            let name = module.name();
            Ok(aliases
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_string()))
        },
    )
}

pub fn validate_imports_for_linking(
    ctx: &HarnessContext, module: &NixModule, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) -> Result<()> {
    let mut resolver = HarnessImportResolver {
        ctx,
        modules,
        aliases,
    };
    linking::validate_imports(module, &mut resolver).context(LinkSnafu)
}

struct HarnessImportResolver<'a> {
    ctx: &'a HarnessContext,
    modules: &'a HashMap<String, HarnessModule>,
    aliases: &'a HashMap<String, String>,
}

impl ImportResolver for HarnessImportResolver<'_> {
    type Error = HarnessResolverError;

    fn resolve_import(
        &mut self, module_name: &str, field_name: &str,
    ) -> std::result::Result<Option<ExternType>, Self::Error> {
        match lookup_import(
            self.ctx,
            self.modules,
            self.aliases,
            module_name,
            field_name,
        ) {
            Ok(import) => Ok(Some(import)),
            Err(Error::UnknownImport { .. } | Error::UnknownImportModule { .. }) => Ok(None),
            Err(err) => Err(HarnessResolverError::from(err)),
        }
    }
}

pub fn active_initializer_traps(
    ctx: &HarnessContext, module: &NixModule, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) -> Result<bool> {
    let mut resolver = HarnessActiveInitializerResolver {
        ctx,
        modules,
        aliases,
    };
    wasm2nix::instantiation::active_initializers_trap(module, &mut resolver)
        .context(ActiveInitializerSnafu)
}

struct HarnessActiveInitializerResolver<'a> {
    ctx: &'a HarnessContext,
    modules: &'a HashMap<String, HarnessModule>,
    aliases: &'a HashMap<String, String>,
}

impl ActiveInitializerResolver for HarnessActiveInitializerResolver<'_> {
    type Error = HarnessResolverError;

    fn memory_min_pages(
        &mut self, module: &NixModule, index: cranelift_wasm::MemoryIndex,
    ) -> std::result::Result<u64, Self::Error> {
        memory_limits_for_index(self.ctx, module, self.modules, self.aliases, index)
            .map(|limits| limits.min)
            .map_err(HarnessResolverError::from)
    }

    fn table_min_elements(
        &mut self, module: &NixModule, index: cranelift_wasm::TableIndex,
    ) -> std::result::Result<u64, Self::Error> {
        table_limits_for_index(self.ctx, module, self.modules, self.aliases, index)
            .map(|limits| limits.min)
            .map_err(HarnessResolverError::from)
    }

    fn imported_global_i32(
        &mut self, module_name: &str, field_name: &str,
    ) -> std::result::Result<u64, Self::Error> {
        if module_name == "spectest" {
            return match field_name {
                "global_i32" => Ok(666),
                _ => Err(HarnessResolverError::from(
                    Error::UnsupportedImportedOffsetGlobal {
                        module_name: module_name.to_string(),
                        field_name: field_name.to_string(),
                    },
                )),
            };
        }

        let target_key = self
            .aliases
            .get(module_name)
            .cloned()
            .unwrap_or_else(|| module_name.to_string());
        let state = self
            .modules
            .get(&target_key)
            .context(UnknownImportModuleSnafu {
                module_name: module_name.to_string(),
            })
            .map_err(HarnessResolverError::from)?
            .state
            .clone();
        let actual = eval_get(
            self.ctx,
            &target_key,
            self.modules,
            self.aliases,
            field_name,
            state.as_deref(),
        )
        .map_err(HarnessResolverError::from)?;
        first_json_u64(&actual.results).map_err(HarnessResolverError::from)
    }
}

fn first_json_u64(value: &JsonValue) -> Result<u64> {
    let JsonValue::Array(items) = value else {
        return ExpectedResultArraySnafu.fail();
    };
    let Some(first) = items.first() else {
        return ExpectedAtLeastOneResultSnafu.fail();
    };
    json_number_to_u64(first)
}

fn memory_limits_for_index(
    ctx: &HarnessContext, module: &NixModule, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, index: cranelift_wasm::MemoryIndex,
) -> Result<Limits> {
    if let Some((module_name, field_name)) = module.info.imported_memories.get(index.index()) {
        match lookup_import(ctx, modules, aliases, module_name, field_name)? {
            ExternType::Memory(limits) => Ok(limits),
            _ => IncompatibleImportTypeSnafu {
                module_name: module_name.clone(),
                field_name: field_name.clone(),
            }
            .fail(),
        }
    } else {
        Ok(Limits::from(&module.info.memories[index].entity))
    }
}

fn table_limits_for_index(
    ctx: &HarnessContext, module: &NixModule, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, index: cranelift_wasm::TableIndex,
) -> Result<Limits> {
    if let Some((module_name, field_name)) = module.info.imported_tables.get(index.index()) {
        match lookup_import(ctx, modules, aliases, module_name, field_name)? {
            ExternType::Table(table) => Ok(table.limits),
            _ => IncompatibleImportTypeSnafu {
                module_name: module_name.clone(),
                field_name: field_name.clone(),
            }
            .fail(),
        }
    } else {
        Ok(TableType::from(&module.info.tables[index].entity).limits)
    }
}

fn lookup_import(
    ctx: &HarnessContext, modules: &HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, module_name: &str, field_name: &str,
) -> Result<ExternType> {
    if module_name == "spectest" {
        return spectest_import(field_name).context(UnknownImportSnafu {
            module_name: module_name.to_string(),
            field_name: field_name.to_string(),
        });
    }

    let target_key = aliases
        .get(module_name)
        .cloned()
        .unwrap_or_else(|| module_name.to_string());
    let module = modules.get(&target_key).context(UnknownImportModuleSnafu {
        module_name: module_name.to_string(),
    })?;

    if let Some(sig) = module.interface.funcs.get(field_name) {
        return Ok(ExternType::Func(sig.clone()));
    }
    if let Some(global) = module.interface.globals.get(field_name) {
        return Ok(ExternType::Global(global.clone()));
    }
    if let Some(index) = module.table_exports.get(field_name) {
        let static_type = module
            .interface
            .tables
            .get(field_name)
            .context(MissingTableLimitsSnafu {
                module_name: module_name.to_string(),
                field_name: field_name.to_string(),
            })?
            .clone();
        let table_type = if let Some(state) = module.state.as_deref() {
            TableType {
                limits: dynamic_table_limits(ctx, state, *index, static_type.limits)?,
                ..static_type
            }
        } else {
            static_type
        };
        return Ok(ExternType::Table(table_type));
    }
    if let Some(&static_limits) = module.interface.memories.get(field_name) {
        let limits = if let Some(state) = module.state.as_deref() {
            dynamic_memory_limits(ctx, state, static_limits)?
        } else {
            static_limits
        };
        return Ok(ExternType::Memory(limits));
    }

    UnknownImportSnafu {
        module_name: module_name.to_string(),
        field_name: field_name.to_string(),
    }
    .fail()
}

fn spectest_import(field_name: &str) -> Option<ExternType> {
    let print = |params: &[&str]| {
        let params = params.iter().map(ToString::to_string).collect();
        Some(ExternType::Func(FuncType::new(params, Vec::new())))
    };
    let global = |ty| Some(ExternType::Global(GlobalType::new(ty, false)));
    match field_name {
        "print" => print(&[]),
        "print_i32" => print(&["i32"]),
        "print_i64" => print(&["i64"]),
        "print_f32" => print(&["f32"]),
        "print_f64" => print(&["f64"]),
        "print_i32_f32" => print(&["i32", "f32"]),
        "print_f64_f64" => print(&["f64", "f64"]),
        "global_i32" => global("i32"),
        "global_i64" => global("i64"),
        "global_f32" => global("f32"),
        "global_f64" => global("f64"),
        "memory" => Some(ExternType::Memory(Limits::new(1, Some(2)))),
        "table" => Some(ExternType::Table(TableType::new(
            "funcref",
            Limits::new(10, Some(20)),
        ))),
        _ => None,
    }
}

fn dynamic_memory_limits(
    ctx: &HarnessContext, state: &str, static_limits: Limits,
) -> Result<Limits> {
    let expr = nix!(harness.dynamicMemoryLimits({
        st = #{state_expr(state)};
        staticMin = #{nix_u64(static_limits.min)?};
        staticMax = #{nix_optional_u64(static_limits.max)?};
    }));
    limits_from_json(eval_nix_json(ctx, &expr)?, static_limits)
}

fn dynamic_table_limits(
    ctx: &HarnessContext, state: &str, index: u32, static_limits: Limits,
) -> Result<Limits> {
    let expr = nix!(harness.dynamicTableLimits({
        st = #{state_expr(state)};
        tableIndex = #{i64::from(index)};
        staticMin = #{nix_u64(static_limits.min)?};
        staticMax = #{nix_optional_u64(static_limits.max)?};
    }));
    limits_from_json(eval_nix_json(ctx, &expr)?, static_limits)
}

fn limits_from_json(value: JsonValue, static_limits: Limits) -> Result<Limits> {
    let JsonValue::Object(map) = value else {
        return LimitsQueryNotObjectSnafu.fail();
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

pub fn write_generated_module(ctx: &HarnessContext, nix: &str) -> Result<PathBuf> {
    ctx.write_temp_nix_file("wast", nix)
}
