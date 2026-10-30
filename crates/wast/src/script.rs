//! Runs a WAST script: directives are checked in order while their Nix work
//! queues up in a [`Batch`], evaluated only when Rust needs a value.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;

use cranelift_entity::EntityRef;
use cranelift_entity::PrimaryMap;
use cranelift_wasm::MemoryIndex;
use cranelift_wasm::TableIndex;
use serde_json::Value as JsonValue;
use snafu::OptionExt;
use snafu::ResultExt;
use snafu::ensure;
use wasm2nix::instantiation::ActiveInitializerResolver;
use wasm2nix::instantiation::active_initializers_trap;
use wasm2nix::linking::ExternType;
use wasm2nix::linking::FuncType;
use wasm2nix::linking::GlobalType;
use wasm2nix::linking::ImportResolver;
use wasm2nix::linking::Limits;
use wasm2nix::linking::LinkError;
use wasm2nix::linking::ModuleInterface;
use wasm2nix::linking::TableType;
use wasm2nix::linking::validate_imports;
use wasm2nix::nix::Expr;
use wasm2nix::nix::nix;
use wasm2nix::translator::Exportable;
use wasm2nix::translator::NixModule;
use wasm2nix::translator::WASM_FEATURES;
use wast::QuoteWat;
use wast::Wast;
use wast::WastDirective;
use wast::WastExecute;
use wast::WastInvoke;
use wast::WastRet;
use wast::Wat;
use wast::lexer::Lexer;
use wast::parser::ParseBuffer;
use wast::parser::{self};
use wast::token::Id;

use crate::error::ActiveInitializerSnafu;
use crate::error::DirectiveSnafu;
use crate::error::EncodeWatSnafu;
use crate::error::Error;
use crate::error::ExpectedResultSnafu;
use crate::error::FailuresSnafu;
use crate::error::HarnessResolverError;
use crate::error::IncompatibleImportTypeSnafu;
use crate::error::LimitsQueryNotObjectSnafu;
use crate::error::LinkSnafu;
use crate::error::LowerNixSnafu;
use crate::error::MissingTableLimitsSnafu;
use crate::error::ModuleAccessBeforeModuleSnafu;
use crate::error::ModuleInstantiationTrappedSnafu;
use crate::error::ParseWastSnafu;
use crate::error::Result;
use crate::error::TranslateWasmSnafu;
use crate::error::UnknownImportModuleSnafu;
use crate::error::UnknownImportSnafu;
use crate::error::UnknownModuleSnafu;
use crate::error::UnsupportedAssertReturnModuleExecutionSnafu;
use crate::error::UnsupportedImportedOffsetGlobalSnafu;
use crate::error::UnsupportedWastDirectiveSnafu;
use crate::eval::Batch;
use crate::eval::Case;
use crate::eval::Check;
use crate::eval::MAX_BATCH_STEPS;
use crate::eval::NewModule;
use crate::eval::Nix;
use crate::eval::Outcome;
use crate::eval::World;
use crate::eval::json_number_to_u64;
use crate::eval::nix_optional_u64;
use crate::eval::nix_u64;
use crate::eval::path_expr;
use crate::expect::Expected;
use crate::expect::results_label;
use crate::expect::results_match;

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

/// A module's current state.
pub enum ModuleState {
    /// A batch variable, settled into a state file by the next flush.
    Pending(Expr),
    /// A state file an earlier flush wrote.
    Settled(PathBuf),
}

impl ModuleState {
    pub fn expr(&self) -> Expr {
        match self {
            Self::Pending(state) => state.clone(),
            Self::Settled(path) => nix!(import(#{path_expr(path)})),
        }
    }
}

pub struct HarnessModule {
    pub path: PathBuf,
    pub state: ModuleState,
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
    fn new(module: &NixModule, written: Written, state: Expr) -> Self {
        let info = &module.info;
        Self {
            path: written.path,
            state: ModuleState::Pending(state),
            imports: written.imports,
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

/// A module written to a Nix file.
struct Written {
    path: PathBuf,
    /// Names of the modules it imports from.
    imports: Vec<String>,
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

    pub const fn world(&self) -> World<'_> {
        World {
            modules: &self.modules,
            aliases: &self.aliases,
        }
    }

    fn directive(&mut self, directive: WastDirective<'_>, at: &str) -> Result<()> {
        if self.batch.step_count() >= MAX_BATCH_STEPS {
            self.flush(None)?;
        }
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
                let what = format!("assert_exhaustion {:?}", call.name);
                self.queue_invoke(&call, case(at, &what, Check::Fail))
                    .map(drop)
            }
            WastDirective::AssertMalformed { module, .. }
            | WastDirective::AssertMalformedCustom { module, .. } => {
                self.assert_rejected(module, at, "assert_malformed");
                Ok(())
            }
            WastDirective::AssertInvalid { module, .. }
            | WastDirective::AssertInvalidCustom { module, .. } => {
                self.assert_rejected(module, at, "assert_invalid");
                Ok(())
            }
            WastDirective::AssertUnlinkable { module, .. } => self.assert_unlinkable(module, at),
            WastDirective::ModuleDefinition(_) => unsupported("module definition"),
            WastDirective::ModuleInstance { .. } => unsupported("module instance"),
            WastDirective::AssertException { .. } => unsupported("assert_exception"),
            WastDirective::AssertSuspension { .. } => unsupported("assert_suspension"),
            WastDirective::Thread(_) => unsupported("thread"),
            WastDirective::Wait { .. } => unsupported("wait"),
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
        let written = self.write_module(&translated)?;
        let key = name.unwrap_or_else(|| self.transient_key("anon"));

        let case = case(at, "module", Check::Succeed);
        let step = self.queue_new_module(&written, &key, NewModule::Instantiate, case);
        self.register(&translated, written, key.clone(), &step);
        self.merge_imported_table_initializers(&key)?;
        self.sync_memory_imports_to_targets(&key)?;
        self.sync_global_imports_from_targets()?;
        self.current = Some(key);
        Ok(())
    }

    /// Queues a step importing the written module as `key`.
    fn queue_new_module(
        &mut self, written: &Written, key: &str, kind: NewModule, case: Case,
    ) -> Expr {
        let expr = self
            .world()
            .new_module_expr(kind, &written.path, key, &written.imports);
        self.batch.step(expr, case)
    }

    /// Adds the module `step` imports as `key`, and returns its state.
    fn register(&mut self, module: &NixModule, written: Written, key: String, step: &Expr) -> Expr {
        let state = self.step_state(step, None);
        self.modules
            .insert(key, HarnessModule::new(module, written, state.clone()));
        state
    }

    /// Queues an invocation; returns the module key and the step variable.
    fn queue_invoke(&mut self, invoke: &WastInvoke<'_>, case: Case) -> Result<(String, Expr)> {
        let key = self.select(invoke.module)?;
        let expr = self.world().invoke_expr(&key, invoke.name, &invoke.args)?;
        let step = self.batch.step(expr, case);
        Ok((key, step))
    }

    /// Queues an invocation that updates the module state.
    fn invoke(&mut self, invoke: &WastInvoke<'_>, case: Case) -> Result<()> {
        let (key, step) = self.queue_invoke(invoke, case)?;
        let prev = self
            .modules
            .get(&key)
            .context(UnknownModuleSnafu { key: key.as_str() })?
            .state
            .expr();
        let state = self.step_state(&step, Some(prev));
        self.set_state(&key, state)?;
        self.sync_after_call(&key)
    }

    /// Queues a read of a global export.
    fn get(&mut self, module: Option<Id<'_>>, global: &str, case: Case) -> Result<()> {
        let key = self.select(module)?;
        self.sync_global_imports_from_targets()?;
        let expr = self.world().get_expr(&key, global)?;
        self.batch.step(expr, case);
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
                let what = format!("assert_return (get {global:?})");
                self.get(module, global, case(at, &what, Check::Return(expected)))
            }
            WastExecute::Wat(_) => UnsupportedAssertReturnModuleExecutionSnafu.fail(),
        }
    }

    fn assert_trap(&mut self, exec: WastExecute<'_>, at: &str) -> Result<()> {
        match exec {
            WastExecute::Invoke(invoke) => {
                let what = format!("assert_trap {:?}", invoke.name);
                self.queue_invoke(&invoke, case(at, &what, Check::Fail))
                    .map(drop)
            }
            WastExecute::Get { module, global, .. } => {
                let what = format!("assert_trap (get {global:?})");
                self.get(module, global, case(at, &what, Check::Fail))
            }
            WastExecute::Wat(wat) => self.assert_trap_module(wat, at),
        }
    }

    /// A module whose instantiation traps still applies the active segments
    /// written before the trap to the tables and memories it imports.
    fn assert_trap_module(&mut self, mut wat: Wat<'_>, at: &str) -> Result<()> {
        let translated = translate(&wat.encode().context(EncodeWatSnafu)?)?;
        self.validate_imports(&translated)?;
        let initializers_trap = self.initializers_trap(&translated)?;
        let written = self.write_module(&translated)?;
        let key = self.transient_key("trap");

        if !initializers_trap {
            let case = case(at, "assert_trap module", Check::Fail);
            self.queue_new_module(&written, &key, NewModule::Instantiate, case);
        }
        let case = case(at, "assert_trap module initial state", Check::Succeed);
        let step = self.queue_new_module(&written, &key, NewModule::InitialState, case);
        let state = self.register(&translated, written, key, &step);
        self.apply_trapping_table_side_effects(&translated, &state)?;
        self.apply_trapping_memory_side_effects(&translated, &state)
    }

    /// `assert_malformed` and `assert_invalid`: validation or translation
    /// must reject the module. Text that does not encode is rejected too.
    fn assert_rejected(&mut self, mut module: QuoteWat<'_>, at: &str, directive: &str) {
        let Ok(wasm) = module.encode() else {
            return;
        };
        let valid = wasmparser::Validator::new_with_features(WASM_FEATURES)
            .validate_all(&wasm)
            .is_ok();
        if valid && NixModule::translate(&wasm).is_ok() {
            self.fail(at, &format!("{directive}: module was accepted"));
        }
    }

    fn assert_unlinkable(&mut self, mut module: Wat<'_>, at: &str) -> Result<()> {
        let translated = translate(&module.encode().context(EncodeWatSnafu)?)?;
        match self.validate_imports(&translated) {
            Ok(()) => {}
            Err(Error::Link {
                source: LinkError::MissingImport { .. } | LinkError::IncompatibleImport { .. },
            }) => return Ok(()),
            Err(err) => return Err(err),
        }
        let written = self.write_module(&translated)?;
        let key = self.transient_key("unlinkable");
        let case = case(at, "assert_unlinkable", Check::Fail);
        self.queue_new_module(&written, &key, NewModule::Instantiate, case);
        Ok(())
    }

    fn fail(&mut self, at: &str, message: &str) {
        self.failures.push(format!("{at}: {message}"));
    }

    fn select(&self, module: Option<Id<'_>>) -> Result<String> {
        module.map_or_else(
            || self.current.clone().context(ModuleAccessBeforeModuleSnafu),
            |module| Ok(self.world().resolve(module.name()).to_string()),
        )
    }

    fn transient_key(&mut self, kind: &str) -> String {
        let key = format!("__{kind}{}", self.next_module);
        self.next_module += 1;
        key
    }

    fn write_module(&mut self, module: &NixModule) -> Result<Written> {
        let nix = wasm2nix::lower::lower_module(module)
            .context(LowerNixSnafu)?
            .render();
        Ok(Written {
            path: self.nix.write("module", &nix)?,
            imports: import_modules(module),
        })
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
            .filter_map(|(key, module)| match &module.state {
                ModuleState::Pending(state) => Some((key.clone(), state.clone())),
                ModuleState::Settled(_) => None,
            })
            .collect();
        let flushed = self.batch.flush(&self.nix, pending, query)?;
        self.failures
            .extend(flushed.outcomes.iter().filter_map(failure));
        for (key, state) in flushed.states {
            let path = self.nix.write_state(&state)?;
            self.modules
                .get_mut(&key)
                .context(UnknownModuleSnafu { key })?
                .state = ModuleState::Settled(path);
        }
        Ok(flushed.query)
    }

    fn validate_imports(&mut self, module: &NixModule) -> Result<()> {
        validate_imports(module, self).context(LinkSnafu)
    }

    fn initializers_trap(&mut self, module: &NixModule) -> Result<bool> {
        active_initializers_trap(module, self).context(ActiveInitializerSnafu)
    }

    fn lookup_import(&mut self, module_name: &str, field_name: &str) -> Result<ExternType> {
        if module_name == "spectest" {
            return spectest_import(field_name).context(UnknownImportSnafu {
                module_name,
                field_name,
            });
        }

        let key = self.world().resolve(module_name).to_string();
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
        let state = module.state.expr();
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
                tableIndex = #{index};
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

    /// The minimum of the limits `limits` reads from the type `import`
    /// resolves to.
    fn imported_min(
        &mut self, (module_name, field_name): &(String, String),
        limits: fn(ExternType) -> Option<Limits>,
    ) -> Result<u64> {
        let import = self.lookup_import(module_name, field_name)?;
        limits(import)
            .map(|limits| limits.min)
            .context(IncompatibleImportTypeSnafu {
                module_name,
                field_name,
            })
    }
}

impl ImportResolver for Session {
    type Error = HarnessResolverError;

    fn resolve_import(
        &mut self, module_name: &str, field_name: &str,
    ) -> std::result::Result<Option<ExternType>, Self::Error> {
        match self.lookup_import(module_name, field_name) {
            Ok(import) => Ok(Some(import)),
            Err(Error::UnknownImport { .. } | Error::UnknownImportModule { .. }) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}

impl ActiveInitializerResolver for Session {
    type Error = HarnessResolverError;

    fn memory_min_pages(
        &mut self, module: &NixModule, index: MemoryIndex,
    ) -> std::result::Result<u64, Self::Error> {
        let Some(import) = module.info.imported_memories.get(index.index()) else {
            return Ok(module.info.memories[index].entity.minimum);
        };
        Ok(self.imported_min(import, |ty| match ty {
            ExternType::Memory(limits) => Some(limits),
            _ => None,
        })?)
    }

    fn table_min_elements(
        &mut self, module: &NixModule, index: TableIndex,
    ) -> std::result::Result<u64, Self::Error> {
        let Some(import) = module.info.imported_tables.get(index.index()) else {
            return Ok(u64::from(module.info.tables[index].entity.minimum));
        };
        Ok(self.imported_min(import, |ty| match ty {
            ExternType::Table(table) => Some(table.limits),
            _ => None,
        })?)
    }

    fn imported_global_i32(
        &mut self, module_name: &str, field_name: &str,
    ) -> std::result::Result<u64, Self::Error> {
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
        let key = self.world().resolve(module_name).to_string();
        ensure!(
            self.modules.contains_key(&key),
            UnknownImportModuleSnafu { module_name }
        );
        let query = self.world().get_expr(&key, field_name)?;
        let value = self.flush(Some(query))?;
        Ok(json_number_to_u64(
            value["results"].get(0).context(ExpectedResultSnafu)?,
        )?)
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

fn unsupported(directive: &'static str) -> Result<()> {
    UnsupportedWastDirectiveSnafu { directive }.fail()
}

fn spectest_import(field_name: &str) -> Option<ExternType> {
    let print = |params: &[&str]| {
        let params = params.iter().map(ToString::to_string).collect();
        Some(ExternType::Func(FuncType::new(params, Vec::new())))
    };
    let global = |ty: &str| Some(ExternType::Global(GlobalType::new(ty.to_string(), false)));
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
            "funcref".to_string(),
            Limits::new(10, Some(20)),
        ))),
        _ => None,
    }
}
