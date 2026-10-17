//! Runs a WAST script: directives are checked in order while their Nix work
//! queues up in a [`Batch`], evaluated only when Rust needs a value.

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use cranelift_entity::{EntityRef, PrimaryMap};
use serde_json::Value as JsonValue;
use snafu::prelude::*;
use wasm2nix::{
    instantiation::ActiveInitializerResolver,
    linking::{
        self, ExternType, FuncType, GlobalType, ImportResolver, Limits, ModuleInterface, TableType,
    },
    nix::{Expr, nix},
    translator::{Exportable, NixModule},
};
use wast::{
    QuoteWat, Wast, WastDirective, WastExecute, WastInvoke, WastRet, Wat,
    lexer::Lexer,
    parser::{self, ParseBuffer},
};

use crate::{
    error::{
        ActiveInitializerSnafu, DirectiveSnafu, EncodeWatSnafu, Error, ExpectedResultSnafu,
        FailuresSnafu, HarnessResolverError, IncompatibleImportTypeSnafu,
        LimitsQueryNotObjectSnafu, LinkSnafu, LowerNixSnafu, MissingTableLimitsSnafu,
        ModuleAccessBeforeModuleSnafu, ModuleInstantiationTrappedSnafu, ParseWastSnafu, Result,
        TranslateWasmSnafu, UnknownImportModuleSnafu, UnknownImportSnafu, UnknownModuleSnafu,
        UnsupportedAssertReturnModuleExecutionSnafu, UnsupportedImportedOffsetGlobalSnafu,
        UnsupportedWastDirectiveSnafu,
    },
    eval::{
        Batch, Case, Check, MAX_BATCH_STEPS, Nix, Outcome, get_expr, initial_state_expr,
        instantiate_expr, invoke_expr, nix_optional_u64, nix_u64,
    },
    expect::{Expected, json_number_to_u64, results_label, results_match},
};

/// Runs a WAST script and checks every assertion in it.
///
/// # Errors
///
/// Returns an error listing every failed assertion, or the first directive the
/// harness could not process.
pub fn run_wast_script(script: &str) -> Result<()> {
    let mut lexer = Lexer::new(script);
    lexer.allow_confusing_unicode(true);
    let buf = ParseBuffer::new_with_lexer(lexer).context(ParseWastSnafu)?;
    let wast = parser::parse::<Wast<'_>>(&buf).context(ParseWastSnafu)?;

    let mut session = Session::new()?;
    for directive in wast.directives {
        if session.batch.steps() >= MAX_BATCH_STEPS {
            session.flush(None)?;
        }
        let (line, col) = directive.span().linecol_in(script);
        let at = format!("{}:{}", line + 1, col + 1);
        session
            .directive(directive, &at)
            .context(DirectiveSnafu { at })?;
    }
    session.flush(None)?;

    ensure!(
        session.failures.is_empty(),
        FailuresSnafu {
            count: session.failures.len(),
            report: session.failures.join("\n"),
        }
    );
    Ok(())
}

pub struct HarnessModule {
    pub path: PathBuf,
    /// The current state: a batch variable while `pending`, otherwise an
    /// import of a settled state file.
    pub state: Expr,
    pub pending: bool,
    /// Names of the modules it imports from.
    pub imports: Vec<String>,
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
    fn new(path: PathBuf, state: Expr, module: &NixModule) -> Self {
        let info = &module.info;
        Self {
            path,
            state,
            pending: true,
            imports: import_modules(module),
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

fn import_modules(module: &NixModule) -> Vec<String> {
    let info = &module.info;
    let mut names = [
        &info.imported_funcs,
        &info.imported_globals,
        &info.imported_memories,
        &info.imported_tables,
    ]
    .into_iter()
    .flatten()
    .map(|(module, _)| module.clone())
    .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
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

pub fn resolve_alias(aliases: &HashMap<String, String>, name: &str) -> String {
    aliases
        .get(name)
        .cloned()
        .unwrap_or_else(|| name.to_string())
}

pub struct Session {
    pub nix: Nix,
    pub batch: Batch,
    pub modules: BTreeMap<String, HarnessModule>,
    pub aliases: HashMap<String, String>,
    current: Option<String>,
    next_module: usize,
    failures: Vec<String>,
}

impl Session {
    fn new() -> Result<Self> {
        Ok(Self {
            nix: Nix::new()?,
            batch: Batch::default(),
            modules: BTreeMap::new(),
            aliases: HashMap::new(),
            current: None,
            next_module: 0,
            failures: Vec::new(),
        })
    }

    fn directive(&mut self, directive: WastDirective<'_>, at: &str) -> Result<()> {
        match directive {
            WastDirective::Module(module) => self.module(module, at),
            WastDirective::Register { name, module, .. } => {
                let key = self.select(module)?;
                self.aliases.insert(name.to_string(), key);
                Ok(())
            }
            WastDirective::Invoke(invoke) => {
                let what = format!("invoke {:?}", invoke.name);
                self.invoke(&invoke, case(at, &what, Check::Succeed))
            }
            WastDirective::AssertReturn { exec, results, .. } => {
                self.assert_return(exec, &results, at)
            }
            WastDirective::AssertTrap { exec, .. } => self.assert_trap(exec, at),
            WastDirective::AssertExhaustion { call, .. } => {
                let key = self.select(call.module)?;
                let expr = invoke_expr(&key, call.name, &call.args, &self.modules, &self.aliases)?;
                let what = format!("assert_exhaustion {:?}", call.name);
                self.batch.step(expr, case(at, &what, Check::Fail));
                Ok(())
            }
            WastDirective::AssertMalformed { module, .. }
            | WastDirective::AssertMalformedCustom { module, .. } => {
                self.assert_malformed(module, at);
                Ok(())
            }
            WastDirective::AssertInvalid { module, .. }
            | WastDirective::AssertInvalidCustom { module, .. } => {
                self.assert_invalid(module, at);
                Ok(())
            }
            WastDirective::AssertUnlinkable { module, .. } => self.assert_unlinkable(module, at),
            WastDirective::ModuleDefinition(_)
            | WastDirective::ModuleInstance { .. }
            | WastDirective::AssertException { .. }
            | WastDirective::AssertSuspension { .. }
            | WastDirective::Thread(_)
            | WastDirective::Wait { .. } => UnsupportedWastDirectiveSnafu.fail(),
        }
    }

    fn module(&mut self, mut module: QuoteWat<'_>, at: &str) -> Result<()> {
        let name = module.name().map(|id| id.name().to_string());
        let translated = translate(&module.encode().context(EncodeWatSnafu)?)?;
        self.validate_imports(&translated)?;
        ensure!(
            !self.initializers_trap(&translated)?,
            ModuleInstantiationTrappedSnafu
        );
        let path = self.write_module(&translated)?;
        let imports = import_modules(&translated);
        let key = name.unwrap_or_else(|| self.transient_key("anon"));

        let expr = instantiate_expr(&path, &key, &imports, &self.modules, &self.aliases);
        let step = self.batch.step(expr, case(at, "module", Check::Succeed));
        let state = self.step_state(&step, None);
        self.modules
            .insert(key.clone(), HarnessModule::new(path, state, &translated));
        self.merge_imported_table_initializers(&key)?;
        self.sync_memory_imports_to_targets(&key);
        self.sync_global_imports_from_targets();
        self.current = Some(key);
        Ok(())
    }

    /// Queues an invocation that updates the module state.
    fn invoke(&mut self, invoke: &WastInvoke<'_>, case: Case) -> Result<()> {
        let key = self.select(invoke.module)?;
        let expr = invoke_expr(
            &key,
            invoke.name,
            &invoke.args,
            &self.modules,
            &self.aliases,
        )?;
        let step = self.batch.step(expr, case);
        let module = self
            .modules
            .get(&key)
            .context(UnknownModuleSnafu { key: key.clone() })?;
        let state = self.step_state(&step, Some(module.state.clone()));
        self.set_state(&key, state);
        self.sync_after_call(&key);
        Ok(())
    }

    fn assert_return(
        &mut self, exec: WastExecute<'_>, results: &[WastRet<'_>], at: &str,
    ) -> Result<()> {
        let expected = results
            .iter()
            .map(Expected::from_ret)
            .collect::<Result<Vec<_>>>()?;
        match exec {
            WastExecute::Invoke(invoke) => {
                let what = format!("assert_return {:?}", invoke.name);
                self.invoke(&invoke, case(at, &what, Check::Return(expected)))
            }
            WastExecute::Get { module, global, .. } => {
                let key = self.select(module)?;
                self.sync_global_imports_from_targets();
                let expr = get_expr(&key, global, &self.modules, &self.aliases)?;
                let what = format!("assert_return (get {global:?})");
                self.batch
                    .step(expr, case(at, &what, Check::Return(expected)));
                Ok(())
            }
            WastExecute::Wat(_) => UnsupportedAssertReturnModuleExecutionSnafu.fail(),
        }
    }

    fn assert_trap(&mut self, exec: WastExecute<'_>, at: &str) -> Result<()> {
        match exec {
            WastExecute::Invoke(invoke) => {
                let key = self.select(invoke.module)?;
                let expr = invoke_expr(
                    &key,
                    invoke.name,
                    &invoke.args,
                    &self.modules,
                    &self.aliases,
                )?;
                let what = format!("assert_trap {:?}", invoke.name);
                self.batch.step(expr, case(at, &what, Check::Fail));
            }
            WastExecute::Get { module, global, .. } => {
                let key = self.select(module)?;
                let expr = get_expr(&key, global, &self.modules, &self.aliases)?;
                let what = format!("assert_trap (get {global:?})");
                self.batch.step(expr, case(at, &what, Check::Fail));
            }
            WastExecute::Wat(wat) => self.assert_trap_module(wat, at)?,
        }
        Ok(())
    }

    /// A module whose instantiation traps still applies the active segments
    /// written before the trap to the tables and memories it imports.
    fn assert_trap_module(&mut self, mut wat: Wat<'_>, at: &str) -> Result<()> {
        let translated = translate(&wat.encode().context(EncodeWatSnafu)?)?;
        self.validate_imports(&translated)?;
        let initializers_trap = self.initializers_trap(&translated)?;
        let path = self.write_module(&translated)?;
        let imports = import_modules(&translated);
        let key = self.transient_key("trap");

        if !initializers_trap {
            let expr = instantiate_expr(&path, &key, &imports, &self.modules, &self.aliases);
            self.batch
                .step(expr, case(at, "assert_trap module", Check::Fail));
        }
        let expr = initial_state_expr(&path, &key, &imports, &self.modules, &self.aliases);
        let step = self.batch.step(
            expr,
            case(at, "assert_trap module initial state", Check::Succeed),
        );
        let state = self.step_state(&step, None);
        self.modules
            .insert(key, HarnessModule::new(path, state.clone(), &translated));
        self.apply_trapping_table_side_effects(&translated, &state);
        self.apply_trapping_memory_side_effects(&translated, &state)
    }

    fn assert_malformed(&mut self, mut module: QuoteWat<'_>, at: &str) {
        let Ok(wasm) = module.encode() else {
            return;
        };
        let valid = wasmparser::Validator::new_with_features(wasm2nix::translator::WASM_FEATURES)
            .validate_all(&wasm)
            .is_ok();
        if valid && NixModule::translate(&wasm).is_ok() {
            self.fail(at, "assert_malformed: module was accepted");
        }
    }

    fn assert_invalid(&mut self, mut module: QuoteWat<'_>, at: &str) {
        let Ok(wasm) = module.encode() else {
            return;
        };
        if NixModule::translate(&wasm).is_ok() {
            self.fail(at, "assert_invalid: module translated successfully");
        }
    }

    fn assert_unlinkable(&mut self, mut module: Wat<'_>, at: &str) -> Result<()> {
        let translated = translate(&module.encode().context(EncodeWatSnafu)?)?;
        if self.validate_imports(&translated).is_err() {
            return Ok(());
        }
        let path = self.write_module(&translated)?;
        let imports = import_modules(&translated);
        let key = self.transient_key("unlinkable");
        let expr = instantiate_expr(&path, &key, &imports, &self.modules, &self.aliases);
        self.batch
            .step(expr, case(at, "assert_unlinkable", Check::Fail));
        Ok(())
    }

    fn fail(&mut self, at: &str, message: &str) {
        self.failures.push(format!("{at} {message}"));
    }

    fn select(&self, module: Option<wast::token::Id<'_>>) -> Result<String> {
        module.map_or_else(
            || self.current.clone().context(ModuleAccessBeforeModuleSnafu),
            |module| Ok(resolve_alias(&self.aliases, module.name())),
        )
    }

    fn transient_key(&mut self, kind: &str) -> String {
        let key = format!("__{kind}{}", self.next_module);
        self.next_module += 1;
        key
    }

    fn write_module(&mut self, module: &NixModule) -> Result<PathBuf> {
        let nix = wasm2nix::lower::lower_module(module)
            .context(LowerNixSnafu)?
            .render();
        self.nix.write("module", &nix)
    }

    /// The state after `step`: its result when it succeeded, `prev` (or a
    /// throwing state) when it failed.
    fn step_state(&mut self, step: &Expr, prev: Option<Expr>) -> Expr {
        let prev = prev.unwrap_or_else(|| nix!(builtins.throw("module failed to instantiate")));
        self.batch.bind(nix!(
            if #{step.clone()}.success then #{step.clone()}.value.st else #{prev}
        ))
    }

    /// Evaluates the queued steps (and `query`, if any), records failed
    /// cases, and settles every pending module state into a state file.
    pub fn flush(&mut self, query: Option<Expr>) -> Result<JsonValue> {
        if self.batch.is_empty() && query.is_none() {
            return Ok(JsonValue::Null);
        }
        let pending = self
            .modules
            .iter()
            .filter(|(_, module)| module.pending)
            .map(|(key, module)| (key.clone(), module.state.clone()))
            .collect();
        let flushed = self.batch.flush(&self.nix, pending, query)?;
        for outcome in flushed.outcomes {
            if let Some(failure) = failure(&outcome) {
                self.failures.push(failure);
            }
        }
        for (key, state) in flushed.states {
            let path = self.nix.write_state(&state)?;
            let module = self
                .modules
                .get_mut(&key)
                .context(UnknownModuleSnafu { key })?;
            module.state = nix!(import(#{Expr::path(path.display().to_string())}));
            module.pending = false;
        }
        Ok(flushed.query)
    }

    fn validate_imports(&mut self, module: &NixModule) -> Result<()> {
        linking::validate_imports(module, &mut Resolver(self)).context(LinkSnafu)
    }

    fn initializers_trap(&mut self, module: &NixModule) -> Result<bool> {
        wasm2nix::instantiation::active_initializers_trap(module, &mut Resolver(self))
            .context(ActiveInitializerSnafu)
    }

    fn lookup_import(&mut self, module_name: &str, field_name: &str) -> Result<ExternType> {
        if module_name == "spectest" {
            return spectest_import(field_name).context(UnknownImportSnafu {
                module_name,
                field_name,
            });
        }

        let key = resolve_alias(&self.aliases, module_name);
        let module = self
            .modules
            .get(&key)
            .context(UnknownImportModuleSnafu { module_name })?;
        if let Some(sig) = module.interface.funcs.get(field_name) {
            return Ok(ExternType::Func(sig.clone()));
        }
        if let Some(global) = module.interface.globals.get(field_name) {
            return Ok(ExternType::Global(global.clone()));
        }
        let state = module.state.clone();
        if let Some(&index) = module.table_exports.get(field_name) {
            let table = module
                .interface
                .tables
                .get(field_name)
                .context(MissingTableLimitsSnafu {
                    module_name,
                    field_name,
                })?
                .clone();
            let query = nix!(harness.dynamicTableLimits({
                st = #{state};
                tableIndex = #{i64::from(index)};
                staticMin = #{nix_u64(table.limits.min)?};
                staticMax = #{nix_optional_u64(table.limits.max)?};
            }));
            let limits = self.query_limits(query, table.limits)?;
            return Ok(ExternType::Table(TableType { limits, ..table }));
        }
        if let Some(&limits) = module.interface.memories.get(field_name) {
            let query = nix!(harness.dynamicMemoryLimits({
                st = #{state};
                staticMin = #{nix_u64(limits.min)?};
                staticMax = #{nix_optional_u64(limits.max)?};
            }));
            return Ok(ExternType::Memory(self.query_limits(query, limits)?));
        }

        UnknownImportSnafu {
            module_name,
            field_name,
        }
        .fail()
    }

    fn query_limits(&mut self, query: Expr, static_limits: Limits) -> Result<Limits> {
        let value = self.flush(Some(query))?;
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

    fn imported_global_i32(&mut self, module_name: &str, field_name: &str) -> Result<u64> {
        if module_name == "spectest" {
            ensure!(
                field_name == "global_i32",
                UnsupportedImportedOffsetGlobalSnafu {
                    module_name,
                    field_name,
                }
            );
            return Ok(666);
        }
        let key = resolve_alias(&self.aliases, module_name);
        ensure!(
            self.modules.contains_key(&key),
            UnknownImportModuleSnafu { module_name }
        );
        let query = get_expr(&key, field_name, &self.modules, &self.aliases)?;
        let value = self.flush(Some(query))?;
        json_number_to_u64(value["results"].get(0).context(ExpectedResultSnafu)?)
    }
}

fn case(at: &str, what: &str, check: Check) -> Case {
    Case {
        at: format!("{at} {what}"),
        check,
    }
}

fn failure(outcome: &Outcome) -> Option<String> {
    let at = &outcome.case.at;
    let message = match (&outcome.case.check, &outcome.results) {
        (Check::Return(expected), Some(actual)) => {
            if results_match(expected, actual) {
                return None;
            }
            format!("expected {}, got {actual}", results_label(expected))
        }
        (Check::Return(_) | Check::Succeed, None) => "failed".to_string(),
        (Check::Fail, Some(_)) => "did not fail".to_string(),
        (Check::Succeed, Some(_)) | (Check::Fail, None) => return None,
    };
    let error = outcome
        .error
        .as_ref()
        .map(|error| format!(":\n{error}"))
        .unwrap_or_default();
    Some(format!("{at}: {message}{error}"))
}

fn translate(wasm: &[u8]) -> Result<NixModule> {
    NixModule::translate(wasm).context(TranslateWasmSnafu)
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

/// Resolves imports and initializer inputs against the session's modules.
struct Resolver<'a>(&'a mut Session);

impl ImportResolver for Resolver<'_> {
    type Error = HarnessResolverError;

    fn resolve_import(
        &mut self, module_name: &str, field_name: &str,
    ) -> std::result::Result<Option<ExternType>, Self::Error> {
        match self.0.lookup_import(module_name, field_name) {
            Ok(import) => Ok(Some(import)),
            Err(Error::UnknownImport { .. } | Error::UnknownImportModule { .. }) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}

impl ActiveInitializerResolver for Resolver<'_> {
    type Error = HarnessResolverError;

    fn memory_min_pages(
        &mut self, module: &NixModule, index: cranelift_wasm::MemoryIndex,
    ) -> std::result::Result<u64, Self::Error> {
        let limits = match module.info.imported_memories.get(index.index()) {
            Some((module_name, field_name)) => {
                match self.0.lookup_import(module_name, field_name)? {
                    ExternType::Memory(limits) => limits,
                    _ => {
                        return Err(IncompatibleImportTypeSnafu {
                            module_name,
                            field_name,
                        }
                        .build()
                        .into());
                    }
                }
            }
            None => Limits::from(&module.info.memories[index].entity),
        };
        Ok(limits.min)
    }

    fn table_min_elements(
        &mut self, module: &NixModule, index: cranelift_wasm::TableIndex,
    ) -> std::result::Result<u64, Self::Error> {
        let limits = match module.info.imported_tables.get(index.index()) {
            Some((module_name, field_name)) => {
                match self.0.lookup_import(module_name, field_name)? {
                    ExternType::Table(table) => table.limits,
                    _ => {
                        return Err(IncompatibleImportTypeSnafu {
                            module_name,
                            field_name,
                        }
                        .build()
                        .into());
                    }
                }
            }
            None => TableType::from(&module.info.tables[index].entity).limits,
        };
        Ok(limits.min)
    }

    fn imported_global_i32(
        &mut self, module_name: &str, field_name: &str,
    ) -> std::result::Result<u64, Self::Error> {
        Ok(self.0.imported_global_i32(module_name, field_name)?)
    }
}
