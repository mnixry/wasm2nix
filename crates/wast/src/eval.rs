//! Nix evaluation. Harness steps queue up in a [`Batch`] and one `nix eval`
//! evaluates all of them; module states stay Nix values between steps.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::env;
use std::env::VarError;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitStatus;

use serde_json::Value as JsonValue;
use snafu::OptionExt;
use snafu::ResultExt;
use tempfile::TempDir;
use wasm2nix::nix::AttrBind;
use wasm2nix::nix::AttrKey;
use wasm2nix::nix::Expr;
use wasm2nix::nix::LetBind;
use wasm2nix::nix::nix;
use wast::WastArg;
use wast::core::WastArgCore;

use crate::error::CreateTempDirSnafu;
use crate::error::DecodeEvalJsonSnafu;
use crate::error::EncodeJsonSnafu;
use crate::error::EvalCmdNotUnicodeSnafu;
use crate::error::EvalNixFailedSnafu;
use crate::error::EvalNixSnafu;
use crate::error::ExpectedIntegerResultSnafu;
use crate::error::InvalidEvalCmdSnafu;
use crate::error::MalformedEvalOutputSnafu;
use crate::error::NixIntegerOverflowSnafu;
use crate::error::Result;
use crate::error::UnknownModuleSnafu;
use crate::error::UnsupportedInvokeArgumentSnafu;
use crate::error::WriteFileSnafu;
use crate::expect::Expected;
use crate::script::HarnessModule;

/// Steps per `nix eval`. Every step's state stays reachable until the
/// evaluation ends, so this bounds evaluator memory.
pub const MAX_BATCH_STEPS: usize = 1024;

/// Overrides [`DEFAULT_EVAL_CMD`] with a shell-quoted command, so
/// other Nix implementations can run the suite.
const EVAL_CMD_VAR: &str = "WASM2NIX_TEST_EVAL_CMD";

/// The `nix eval` invocation, before the flags the output parsing relies on.
const DEFAULT_EVAL_CMD: &[&str] = &[
    "nix",
    "--extra-experimental-features",
    "nix-command flakes",
    "eval",
    "--store",
    "dummy://",
];

/// The Nix files of one script run: the embedded runtime and harness, the
/// generated modules, settled states, and the evaluated program.
pub struct Nix {
    dir: TempDir,
    files: usize,
    program: String,
    args: Vec<String>,
}

impl Nix {
    pub fn new() -> Result<Self> {
        let (program, args) = eval_cmd()?;
        let nix = Self {
            dir: TempDir::new().context(CreateTempDirSnafu)?,
            files: 0,
            program,
            args,
        };
        write_file(&nix.rt_path(), wasm2nix_runtime::runtime_nix())?;
        write_file(&nix.harness_path(), include_str!("../nix/harness.nix"))?;
        Ok(nix)
    }

    fn rt_path(&self) -> PathBuf {
        self.dir.path().join("rt.nix")
    }

    fn harness_path(&self) -> PathBuf {
        self.dir.path().join("harness.nix")
    }

    /// Writes a new Nix file; importing it is memoized by the evaluator.
    #[expect(clippy::arithmetic_side_effects, reason = "one file per directive")]
    pub fn write(&mut self, kind: &str, nix: &str) -> Result<PathBuf> {
        let path = self.dir.path().join(format!("{kind}-{}.nix", self.files));
        self.files += 1;
        write_file(&path, nix)?;
        Ok(path)
    }

    /// Writes a settled module state; a JSON `null` marks a state that failed
    /// to evaluate.
    pub fn write_state(&mut self, state: &JsonValue) -> Result<PathBuf> {
        let nix = if state.is_null() {
            nix!(builtins.throw("module state failed to evaluate in an earlier step"))
        } else {
            let json = serde_json::to_string(state).context(EncodeJsonSnafu)?;
            nix!(builtins.fromJSON(#{json}))
        };
        self.write("state", &nix.render())
    }

    /// Evaluates `body` to JSON with `binds`, `rt` and `harness` in scope.
    fn eval(&self, binds: &[LetBind], body: &Expr) -> Result<Result<JsonValue, EvalFailure>> {
        let prelude = [
            LetBind::new("rt", import_expr(&self.rt_path(), nix!({}))),
            LetBind::new("harness", import_expr(&self.harness_path(), nix!({}))),
        ];
        // Rendered bind by bind, so the batch's bindings need no copy.
        let mut program = String::from("let\n");
        for bind in prelude.iter().chain(binds) {
            program.push_str(&bind.name);
            program.push_str(" = ");
            program.push_str(&bind.value.render());
            program.push_str(";\n");
        }
        program.push_str("in builtins.toJSON (");
        program.push_str(&body.render());
        program.push_str(")\n");
        let path = self.dir.path().join("eval.nix");
        write_file(&path, &program)?;
        let output = Command::new(&self.program)
            .args(&self.args)
            .args(["--raw", "--impure", "--log-format", "internal-json", "-f"])
            .arg(&path)
            .output()
            .context(EvalNixSnafu)?;
        if !output.status.success() {
            return Ok(Err(EvalFailure::from_log(
                output.status,
                &String::from_utf8_lossy(&output.stderr),
            )));
        }
        serde_json::from_slice(&output.stdout)
            .context(DecodeEvalJsonSnafu)
            .map(Ok)
    }
}

/// The eval command: [`EVAL_CMD_VAR`] when set, else [`DEFAULT_EVAL_CMD`].
fn eval_cmd() -> Result<(String, Vec<String>)> {
    let cmd = match env::var(EVAL_CMD_VAR) {
        Ok(cmd) => shlex::split(&cmd).context(InvalidEvalCmdSnafu)?,
        Err(VarError::NotPresent) => DEFAULT_EVAL_CMD.iter().map(ToString::to_string).collect(),
        Err(VarError::NotUnicode(_)) => return EvalCmdNotUnicodeSnafu.fail(),
    };
    let mut cmd = cmd.into_iter();
    let program = cmd.next().context(InvalidEvalCmdSnafu)?;
    Ok((program, cmd.collect()))
}

/// A failed evaluation, read from Nix's `internal-json` log.
struct EvalFailure {
    status: ExitStatus,
    /// The step that was started but never finished.
    aborted_step: Option<usize>,
    /// The innermost error message.
    error: String,
}

impl EvalFailure {
    fn from_log(status: ExitStatus, log: &str) -> Self {
        let mut open = Vec::new();
        let mut error = None;
        let records = log
            .lines()
            .filter_map(|line| line.strip_prefix("@nix "))
            .filter_map(|record| serde_json::from_str::<JsonValue>(record).ok())
            .filter(|record| record.get("action").and_then(JsonValue::as_str) == Some("msg"));
        for record in records {
            if let Some(raw) = record.get("raw_msg").and_then(JsonValue::as_str) {
                error = Some(raw.to_string());
                continue;
            }
            let Some(marker) = record
                .get("msg")
                .and_then(JsonValue::as_str)
                .and_then(|msg| msg.strip_prefix("trace: "))
                .and_then(|trace| serde_json::from_str::<JsonValue>(trace).ok())
            else {
                continue;
            };
            let Some(id) = marker
                .get("id")
                .and_then(JsonValue::as_u64)
                .and_then(|id| usize::try_from(id).ok())
            else {
                continue;
            };
            match marker.get("wast").and_then(JsonValue::as_str) {
                Some("step") => open.push(id),
                Some("done") => open.retain(|open| *open != id),
                _ => {}
            }
        }
        Self {
            status,
            aborted_step: open.last().copied(),
            error: error.unwrap_or_else(|| log.to_string()),
        }
    }
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).context(WriteFileSnafu { path })
}

/// What a step must do for its WAST case to pass.
#[derive(Debug)]
pub enum Check {
    Return(Vec<Expected>),
    Succeed,
    Fail,
}

#[derive(Debug)]
pub struct Case {
    /// `line:col directive`, for the failure report.
    pub at: String,
    pub check: Check,
}

impl Check {
    const fn wants_success(&self) -> bool {
        !matches!(self, Self::Fail)
    }
}

struct Step {
    /// The index of the step's binding in `Batch::binds`.
    bind: usize,
    /// The variable the step is bound to.
    var: String,
    expr: Expr,
    case: Case,
}

/// The outcome of one step: its results when it succeeded, and the Nix error
/// when one was captured.
pub struct Outcome {
    pub case: Case,
    pub results: Option<JsonValue>,
    pub error: Option<String>,
}

pub struct Flushed {
    pub outcomes: Vec<Outcome>,
    pub states: BTreeMap<String, JsonValue>,
    pub query: JsonValue,
}

/// Pending top-level bindings of the next `nix eval`: steps, and the module
/// states derived from them.
#[derive(Default)]
pub struct Batch {
    binds: Vec<LetBind>,
    steps: Vec<Step>,
}

impl Batch {
    pub const fn step_count(&self) -> usize {
        self.steps.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.binds.is_empty()
    }

    /// Binds `expr` to a fresh variable and returns the variable.
    pub fn bind(&mut self, expr: Expr) -> Expr {
        let var = format!("v{}", self.binds.len());
        self.binds.push(LetBind::new(var.clone(), expr));
        Expr::ident(var)
    }

    /// Queues `expr`, a `{ results; st; }` set, as a step. Returns the
    /// `tryEval` result variable.
    pub fn step(&mut self, expr: Expr, case: Case) -> Expr {
        let id = step_id(self.steps.len());
        let bind = self.binds.len();
        let var = self.bind(nix!(harness.attempt(#{id}, #{expr.clone()})));
        self.steps.push(Step {
            bind,
            var: var.to_string(),
            expr,
            case,
        });
        var
    }

    /// Evaluates every queued step, the given module `states` and `query` in
    /// one `nix eval`.
    ///
    /// A step whose failure `tryEval` cannot catch (stack overflow, infinite
    /// recursion) aborts the evaluation. Its trace markers identify it; it is
    /// then replaced by a failed result and the batch evaluated again.
    ///
    /// `tryEval` hides why a step failed, so the first step that unexpectedly
    /// failed is forced once more without it to capture the Nix error.
    pub fn flush(
        &mut self, nix: &Nix, states: Vec<(String, Expr)>, query: Option<Expr>,
    ) -> Result<Flushed> {
        let outcomes = Expr::list(
            self.steps
                .iter()
                .map(|step| {
                    let var = Expr::ident(step.var.as_str());
                    nix!(if #{var.clone()}.success then #{var}.value.results else null)
                })
                .collect(),
        );
        let states = Expr::attrset(states.into_iter().map(|(key, state)| {
            AttrBind::new(AttrKey::string(key), nix!(harness.settle(#{state})))
        }));
        let body = nix!({
            outcomes = #{outcomes};
            states = #{states};
            query = #{query.unwrap_or_else(Expr::null)};
        });

        let mut errors = BTreeMap::new();
        let mut value = loop {
            match nix.eval(&self.binds, &body)? {
                Ok(value) => break value,
                Err(failure) => {
                    let step = failure
                        .aborted_step
                        .filter(|step| !errors.contains_key(step))
                        .context(EvalNixFailedSnafu {
                            status: failure.status,
                            stderr: failure.error.clone(),
                        })?;
                    *self.step_value(step) = nix!({
                        success = false;
                        value = null;
                    });
                    errors.insert(step, failure.error);
                }
            }
        };
        let results = match value.get_mut("outcomes").map(JsonValue::take) {
            Some(JsonValue::Array(results)) if results.len() == self.steps.len() => results,
            _ => return MalformedEvalOutputSnafu { field: "outcomes" }.fail(),
        };

        let unexpected = self.steps.iter().enumerate().find(|(id, step)| {
            step.case.check.wants_success()
                && results.get(*id).is_none_or(JsonValue::is_null)
                && !errors.contains_key(id)
        });
        if let Some((id, step)) = unexpected {
            let var = Expr::ident(step.var.as_str());
            let unguarded = nix!(harness.unguarded(#{step_id(id)}, #{step.expr.clone()}));
            *self.step_value(id) = unguarded;
            if let Err(failure) = nix.eval(&self.binds, &nix!(#{var}.success))? {
                errors.insert(id, failure.error);
            }
        }

        self.binds.clear();
        let outcomes = std::mem::take(&mut self.steps)
            .into_iter()
            .zip(results)
            .enumerate()
            .map(|(id, (step, results))| Outcome {
                case: step.case,
                results: (!results.is_null()).then_some(results),
                error: errors.remove(&id),
            })
            .collect();
        let Some(JsonValue::Object(states)) = value.get_mut("states").map(JsonValue::take) else {
            return MalformedEvalOutputSnafu { field: "states" }.fail();
        };
        Ok(Flushed {
            outcomes,
            states: states.into_iter().collect(),
            query: value
                .get_mut("query")
                .map(JsonValue::take)
                .unwrap_or_default(),
        })
    }
}

impl Batch {
    /// The value bound for step `step`.
    #[expect(
        clippy::indexing_slicing,
        reason = "steps are created with their bindings and index them"
    )]
    fn step_value(&mut self, step: usize) -> &mut Expr {
        let bind = self.steps[step].bind;
        &mut self.binds[bind].value
    }
}

fn step_id(index: usize) -> i64 {
    i64::try_from(index).expect("step count fits in i64")
}

/// What a step importing a new module evaluates to.
#[derive(Debug, Clone, Copy)]
pub enum NewModule {
    /// The state after instantiation.
    Instantiate,
    /// The state before active segments and the start function run.
    InitialState,
}

/// The modules a step can import, and the aliases they are registered
/// under.
#[derive(Clone, Copy)]
pub struct World<'a> {
    pub modules: &'a BTreeMap<String, HarnessModule>,
    pub aliases: &'a HashMap<String, String>,
}

impl<'a> World<'a> {
    /// The module key that `name`, a registered alias or a key, refers to.
    pub fn resolve<'s>(&self, name: &'s str) -> &'s str
    where
        'a: 's, {
        self.aliases.get(name).map_or(name, String::as_str)
    }

    /// A step importing the module at `path` as `key`; `imports` are the
    /// module names it imports from.
    pub fn new_module_expr(
        &self, kind: NewModule, path: &Path, key: &str, imports: &[String],
    ) -> Expr {
        let roots = imports.iter().map(|name| self.resolve(name));
        let (mut binds, _) = self.binds(roots);
        binds.push(LetBind::new("m_new", import_module_expr(path, key)));
        let body = match kind {
            NewModule::Instantiate => nix!({
                results = [];
                st = (m_new.instantiate({})).st;
            }),
            NewModule::InitialState => nix!({
                results = [];
                st = m_new."__wasm2nix"."st0";
            }),
        };
        Expr::let_in(binds, body)
    }

    /// A step invoking export `name` of module `key`.
    pub fn invoke_expr(&self, key: &str, name: &str, args: &[WastArg<'_>]) -> Result<Expr> {
        let mut args = args
            .iter()
            .map(wast_arg_to_nix)
            .collect::<Result<Vec<_>>>()?;
        args.push(Expr::ident("st_in"));
        let (mut binds, export) = self.export(key, name)?;
        binds.push(LetBind::new("call", Expr::app(export, args)));
        Ok(Expr::let_in(
            binds,
            nix!({
                results = call.results;
                st = call.st;
            }),
        ))
    }

    /// A step reading global export `name` of module `key`.
    pub fn get_expr(&self, key: &str, name: &str) -> Result<Expr> {
        let (mut binds, export) = self.export(key, name)?;
        binds.push(LetBind::new("value", nix!(#{export}.get(st_in))));
        Ok(Expr::let_in(
            binds,
            nix!({
                results = [value];
                st = st_in;
            }),
        ))
    }

    /// The world of module `key` with its state bound to `st_in`, and the
    /// expression of its export `name`.
    fn export(&self, key: &str, name: &str) -> Result<(Vec<LetBind>, Expr)> {
        let state = self
            .modules
            .get(key)
            .context(UnknownModuleSnafu { key })?
            .state
            .expr();
        let (mut binds, vars) = self.binds([key]);
        let module = Expr::ident(vars.get(key).context(UnknownModuleSnafu { key })?.as_str());
        let export_key = format!("__wasm2nix_export_{}", hex::encode(name.as_bytes()));
        binds.push(LetBind::new("st_in", state));
        Ok((
            binds,
            nix!(#{module}."__wasm2nix"."exportsByName".(export_key)),
        ))
    }

    /// Bindings importing, with their current states, the modules connected
    /// to `roots` by imports in either direction: a step can only reach
    /// those, and the function registry needs every module whose functions
    /// can land in a shared table. Also returns the variable bound to each
    /// module key.
    fn binds<'k>(
        &self, roots: impl IntoIterator<Item = &'k str>,
    ) -> (Vec<LetBind>, HashMap<String, String>) {
        let mut neighbours = HashMap::<&str, Vec<&str>>::new();
        for (key, module) in self.modules {
            for name in &module.imports {
                if let Some((target, _)) = self.modules.get_key_value(self.resolve(name)) {
                    neighbours.entry(key).or_default().push(target);
                    neighbours.entry(target).or_default().push(key);
                }
            }
        }
        let mut component = BTreeSet::new();
        let mut queue = roots
            .into_iter()
            .filter_map(|root| {
                self.modules
                    .get_key_value(root)
                    .map(|(key, _)| key.as_str())
            })
            .collect::<Vec<_>>();
        while let Some(key) = queue.pop() {
            if component.insert(key) {
                queue.extend(neighbours.get(key).into_iter().flatten());
            }
        }

        // Each module is bound raw as `m<i>` and with its state as
        // `m<i>_import`.
        let members = component
            .iter()
            .filter_map(|key| self.modules.get_key_value(*key))
            .enumerate()
            .map(|(i, (key, module))| (key, module, format!("m{i}"), format!("m{i}_import")))
            .collect::<Vec<_>>();
        let vars = members
            .iter()
            .map(|(key, _, _, var)| ((*key).clone(), var.clone()))
            .collect::<HashMap<_, _>>();
        let func_registry = Expr::attrset(members.iter().map(|(key, _, _, var)| {
            let module = Expr::ident(var.as_str());
            AttrBind::new(
                AttrKey::string(key.as_str()),
                nix!(#{module}."__wasm2nix"."funcs"),
            )
        }));
        let mut binds = vec![
            LetBind::new("imports", self.imports_expr(&vars)),
            LetBind::new("funcRegistry", func_registry),
        ];
        for (key, module, raw, var) in members {
            binds.push(LetBind::new(
                raw.as_str(),
                import_module_expr(&module.path, key),
            ));
            binds.push(LetBind::new(
                var,
                nix!(merge(#{Expr::ident(raw)}, {
                    "__wasm2nixState" = #{module.state.expr()};
                })),
            ));
        }
        (binds, vars)
    }

    /// The import set shared by every module of a world: each module, by key
    /// and by registered alias, plus `spectest` and the function registry.
    fn imports_expr(&self, vars: &HashMap<String, String>) -> Expr {
        let mut fields = BTreeMap::new();
        fields.insert("__funcRegistry", Expr::ident("funcRegistry"));
        fields.insert("spectest", nix!(harness.spectest));
        for (key, var) in vars {
            fields.insert(key, Expr::ident(var.as_str()));
        }
        for (alias, key) in self.aliases {
            if let Some(var) = vars.get(key) {
                fields.insert(alias, Expr::ident(var.as_str()));
            }
        }
        Expr::attrset(
            fields
                .into_iter()
                .map(|(name, value)| AttrBind::new(AttrKey::string(name), value)),
        )
    }
}

pub fn path_expr(path: &Path) -> Expr {
    Expr::path(path.display().to_string())
}

fn import_expr(path: &Path, arg: Expr) -> Expr {
    nix!(import(#{path_expr(path)}, #{arg}))
}

/// Imports the module at `path` as `key`, with the world's shared imports.
fn import_module_expr(path: &Path, key: &str) -> Expr {
    nix!(import(#{path_expr(path)}, rt, merge(imports, {
        "__moduleId" = #{key};
    })))
}

fn wast_arg_to_nix(arg: &WastArg<'_>) -> Result<Expr> {
    match arg {
        WastArg::Core(WastArgCore::I32(value)) => Ok(Expr::int(i64::from(value.cast_unsigned()))),
        WastArg::Core(WastArgCore::I64(value)) => Ok(Expr::int(*value)),
        WastArg::Core(WastArgCore::F32(value)) => Ok(Expr::int(i64::from(value.bits))),
        WastArg::Core(WastArgCore::F64(value)) => Ok(Expr::int(value.bits.cast_signed())),
        WastArg::Core(WastArgCore::RefExtern(value)) => {
            Ok(nix!({ extern = #{i64::from(*value)}; }))
        }
        WastArg::Core(WastArgCore::RefNull(_)) => Ok(Expr::int(0)),
        _ => UnsupportedInvokeArgumentSnafu {
            arg: format!("{arg:?}"),
        }
        .fail(),
    }
}

/// A Nix integer read back as the u64 bits it encodes.
pub fn json_number_to_u64(value: &JsonValue) -> Result<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().map(i64::cast_unsigned))
        .context(ExpectedIntegerResultSnafu)
}

pub fn nix_u64(value: u64) -> Result<Expr> {
    let value = i64::try_from(value).context(NixIntegerOverflowSnafu { value })?;
    Ok(Expr::int(value))
}

pub fn nix_optional_u64(value: Option<u64>) -> Result<Expr> {
    value
        .map(nix_u64)
        .transpose()
        .map(|value| value.unwrap_or_else(Expr::null))
}
