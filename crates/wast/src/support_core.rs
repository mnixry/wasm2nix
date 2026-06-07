use std::{collections::HashMap, path::PathBuf};

use cranelift_entity::EntityRef;
use serde_json::Value as JsonValue;
use snafu::prelude::*;
use tempfile::{Builder as TempFileBuilder, TempDir};
use wasm2nix::{
    instantiation::ActiveInitializerResolver,
    linking::{self, ExternType, FuncType, GlobalType, ImportResolver, Limits, TableType},
    nix::nix,
    translator::NixModule,
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
        LowerNixSnafu, MissingMemoryLimitsSnafu, MissingTableLimitsSnafu,
        ModuleAccessBeforeModuleSnafu, ParseWastSnafu, Result, TranslateWasmSnafu,
        UnknownImportModuleSnafu, UnknownImportSnafu, UnknownModuleSnafu, WriteTempFileSnafu,
    },
    support_eval::{
        eval_get, eval_instantiate, eval_invoke, eval_nix_json, nix_optional_u64, nix_u64,
        state_expr,
    },
    support_state::{
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
                let imported_tables = translated.info.imported_tables.clone();
                let imported_globals = translated.info.imported_globals.clone();
                let imported_memories = translated.info.imported_memories.clone();
                error::directive(
                    directive_index,
                    validate_imports_for_linking(&ctx, &translated, &modules, &aliases),
                )?;
                let func_exports = func_exports(&translated);
                let table_exports = table_exports(&translated);
                let global_exports = global_exports(&translated);
                let memory_exports = memory_exports(&translated);
                let table_export_limits = table_export_limits(&translated);
                let global_export_types = global_export_types(&translated);
                let memory_export_limits = memory_export_limits(&translated);
                if active_initializer_traps(&ctx, &translated, &modules, &aliases)? {
                    return Err(Error::Directive {
                        directive_index,
                        source: Box::new(Error::ModuleInstantiationTrapped),
                    });
                }
                let nix = error::directive(
                    directive_index,
                    wasm2nix::lowerings::lower_module(&translated).context(LowerNixSnafu),
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
                if wasmparser::Validator::new_with_features(harness_wasm_features())
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
                    wasm2nix::lowerings::lower_module(&translated).context(LowerNixSnafu),
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

const fn harness_wasm_features() -> wasmparser::WasmFeatures {
    wasmparser::WasmFeatures::WASM1
        .union(wasmparser::WasmFeatures::MUTABLE_GLOBAL)
        .union(wasmparser::WasmFeatures::SIGN_EXTENSION)
        .union(wasmparser::WasmFeatures::MULTI_VALUE)
        .union(wasmparser::WasmFeatures::REFERENCE_TYPES)
        .union(wasmparser::WasmFeatures::SATURATING_FLOAT_TO_INT)
        .union(wasmparser::WasmFeatures::BULK_MEMORY)
}

pub struct HarnessModule {
    pub path: PathBuf,
    pub state: Option<String>,
    pub imported_tables: Vec<(String, String)>,
    pub imported_globals: Vec<(String, String)>,
    pub imported_memories: Vec<(String, String)>,
    pub func_exports: HashMap<String, FuncType>,
    pub table_exports: HashMap<String, u32>,
    pub global_exports: HashMap<String, u32>,
    pub memory_exports: HashMap<String, u32>,
    pub table_export_limits: HashMap<String, TableType>,
    pub global_export_types: HashMap<String, GlobalType>,
    pub memory_export_limits: HashMap<String, Limits>,
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
        let memory = module.info.memories[index].entity;
        Ok(Limits::new(memory.minimum, memory.maximum))
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
        let table = module.info.tables[index].entity;
        Ok(Limits::new(
            u64::from(table.minimum),
            table.maximum.map(u64::from),
        ))
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

    if let Some(sig) = module.func_exports.get(field_name) {
        return Ok(ExternType::Func(sig.clone()));
    }
    if let Some(global) = module.global_export_types.get(field_name) {
        return Ok(ExternType::Global(global.clone()));
    }
    if let Some(index) = module.table_exports.get(field_name) {
        let static_type = module
            .table_export_limits
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
    if let Some(index) = module.memory_exports.get(field_name) {
        let static_limits =
            *module
                .memory_export_limits
                .get(field_name)
                .context(MissingMemoryLimitsSnafu {
                    module_name: module_name.to_string(),
                    field_name: field_name.to_string(),
                })?;
        let limits = if let Some(state) = module.state.as_deref() {
            dynamic_memory_limits(ctx, state, *index, static_limits)?
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
    match field_name {
        "print" => Some(ExternType::Func(FuncType::new(Vec::new(), Vec::new()))),
        "print_i32" => Some(ExternType::Func(FuncType::new(
            vec!["i32".to_string()],
            Vec::new(),
        ))),
        "print_i64" => Some(ExternType::Func(FuncType::new(
            vec!["i64".to_string()],
            Vec::new(),
        ))),
        "print_f32" => Some(ExternType::Func(FuncType::new(
            vec!["f32".to_string()],
            Vec::new(),
        ))),
        "print_f64" => Some(ExternType::Func(FuncType::new(
            vec!["f64".to_string()],
            Vec::new(),
        ))),
        "print_i32_f32" => Some(ExternType::Func(FuncType::new(
            vec!["i32".to_string(), "f32".to_string()],
            Vec::new(),
        ))),
        "print_f64_f64" => Some(ExternType::Func(FuncType::new(
            vec!["f64".to_string(), "f64".to_string()],
            Vec::new(),
        ))),
        "global_i32" => Some(ExternType::Global(GlobalType::new("i32", false))),
        "global_i64" => Some(ExternType::Global(GlobalType::new("i64", false))),
        "global_f32" => Some(ExternType::Global(GlobalType::new("f32", false))),
        "global_f64" => Some(ExternType::Global(GlobalType::new("f64", false))),
        "memory" => Some(ExternType::Memory(Limits::new(1, Some(2)))),
        "table" => Some(ExternType::Table(TableType::new(
            "funcref",
            Limits::new(10, Some(20)),
        ))),
        _ => None,
    }
}

fn dynamic_memory_limits(
    ctx: &HarnessContext, state: &str, _index: u32, static_limits: Limits,
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

pub fn table_exports(module: &NixModule) -> HashMap<String, u32> {
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

pub fn func_exports(module: &NixModule) -> HashMap<String, FuncType> {
    module
        .info
        .functions
        .iter()
        .flat_map(|(_idx, func)| {
            let sig = FuncType::from_signature(&module.info.signatures[func.entity]);
            func.export_names
                .iter()
                .cloned()
                .map(move |name| (name, sig.clone()))
        })
        .collect()
}

pub fn global_exports(module: &NixModule) -> HashMap<String, u32> {
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

pub fn global_export_types(module: &NixModule) -> HashMap<String, GlobalType> {
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

pub fn memory_exports(module: &NixModule) -> HashMap<String, u32> {
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

pub fn memory_export_limits(module: &NixModule) -> HashMap<String, Limits> {
    module
        .info
        .memories
        .iter()
        .flat_map(|(_idx, memory)| {
            let limits = Limits::new(memory.entity.minimum, memory.entity.maximum);
            memory
                .export_names
                .iter()
                .cloned()
                .map(move |name| (name, limits))
        })
        .collect()
}

pub fn table_export_limits(module: &NixModule) -> HashMap<String, TableType> {
    module
        .info
        .tables
        .iter()
        .flat_map(|(_idx, table)| {
            let table_type = TableType::new(
                table.entity.wasm_ty.to_string(),
                Limits::new(
                    u64::from(table.entity.minimum),
                    table.entity.maximum.map(u64::from),
                ),
            );
            table
                .export_names
                .iter()
                .cloned()
                .map(move |name| (name, table_type.clone()))
        })
        .collect()
}

fn global_import_type(global: &cranelift_wasm::Global) -> GlobalType {
    GlobalType::new(global.wasm_ty.to_string(), global.mutability)
}
