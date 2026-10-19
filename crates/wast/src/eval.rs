//! Nix evaluation. Harness steps queue up in a [`Batch`] and one `nix eval`
//! evaluates all of them; module states stay Nix values between steps.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value as JsonValue;
use snafu::prelude::*;
use tempfile::TempDir;
use wasm2nix::nix::{AttrKey, Expr, nix};
use wast::{WastArg, core::WastArgCore};

use crate::{
    error::{
        CreateTempDirSnafu, DecodeEvalJsonSnafu, EncodeJsonSnafu, EvalNixFailedSnafu, EvalNixSnafu,
        InvalidEvalCmdSnafu, NixIntegerOverflowSnafu, Result, UnknownModuleSnafu,
        UnsupportedInvokeArgumentSnafu, WriteFileSnafu,
    },
    expect::Expected,
    script::{HarnessModule, resolve_alias},
};

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
}

impl Nix {
    pub fn new() -> Result<Self> {
        let nix = Self {
            dir: TempDir::new().context(CreateTempDirSnafu)?,
            files: 0,
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

    /// Evaluates `body` to JSON with `rt` and `harness` in scope.
    fn eval(
        &self, binds: Vec<(String, Expr)>, body: Expr,
    ) -> Result<Result<JsonValue, EvalFailure>> {
        let mut all = vec![
            ("rt".into(), import_expr(&self.rt_path(), nix!({}))),
            (
                "harness".into(),
                import_expr(&self.harness_path(), nix!({})),
            ),
        ];
        all.extend(binds);
        let program = Expr::let_in(all, nix!(builtins.toJSON(#{body})));
        let path = self.dir.path().join("eval.nix");
        write_file(&path, &program.render())?;
        let cmd = env::var(EVAL_CMD_VAR).map_or_else(
            |_| Some(DEFAULT_EVAL_CMD.iter().copied().map(Into::into).collect()),
            |cmd| shlex::split(&cmd),
        );
        let (program, args) = cmd
            .as_deref()
            .and_then(<[String]>::split_first)
            .context(InvalidEvalCmdSnafu)?;
        let output = Command::new(program)
            .args(args)
            .args(["--raw", "--impure", "--log-format", "internal-json", "-f"])
            .arg(&path)
            .output()
            .context(EvalNixSnafu)?;
        if !output.status.success() {
            return Ok(Err(EvalFailure::from_log(&String::from_utf8_lossy(
                &output.stderr,
            ))));
        }
        serde_json::from_slice(&output.stdout)
            .context(DecodeEvalJsonSnafu)
            .map(Ok)
    }
}

/// A failed evaluation, read from Nix's `internal-json` log.
struct EvalFailure {
    /// The step that was started but never finished.
    aborted_step: Option<usize>,
    /// The innermost error message.
    error: String,
}

impl EvalFailure {
    fn from_log(log: &str) -> Self {
        let mut open = Vec::new();
        let mut error = None;
        let records = log
            .lines()
            .filter_map(|line| line.strip_prefix("@nix "))
            .filter_map(|record| serde_json::from_str::<JsonValue>(record).ok())
            .filter(|record| record["action"] == "msg");
        for record in records {
            if let Some(raw) = record["raw_msg"].as_str() {
                error = Some(raw.to_string());
                continue;
            }
            let Some(marker) = record["msg"]
                .as_str()
                .and_then(|msg| msg.strip_prefix("trace: "))
                .and_then(|trace| serde_json::from_str::<JsonValue>(trace).ok())
            else {
                continue;
            };
            let Some(id) = marker["id"]
                .as_u64()
                .and_then(|id| usize::try_from(id).ok())
            else {
                continue;
            };
            match marker["wast"].as_str() {
                Some("step") => open.push(id),
                Some("done") => open.retain(|open| *open != id),
                _ => {}
            }
        }
        Self {
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
    bind: usize,
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
    binds: Vec<(String, Expr)>,
    steps: Vec<Step>,
}

impl Batch {
    pub const fn steps(&self) -> usize {
        self.steps.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.binds.is_empty()
    }

    /// Binds `expr` to a fresh variable and returns the variable.
    pub fn bind(&mut self, expr: Expr) -> Expr {
        let var = format!("v{}", self.binds.len());
        self.binds.push((var.clone(), expr));
        Expr::ident(var)
    }

    /// Queues `expr`, a `{ results; st; }` set, as a step. Returns the
    /// `tryEval` result variable.
    pub fn step(&mut self, expr: Expr, case: Case) -> Expr {
        let id = step_id(self.steps.len());
        self.steps.push(Step {
            bind: self.binds.len(),
            expr: expr.clone(),
            case,
        });
        self.bind(nix!(harness.attempt(#{id}, #{expr})))
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
                    let var = Expr::ident(self.binds[step.bind].0.as_str());
                    nix!(if #{var.clone()}.success then #{var}.value.results else null)
                })
                .collect(),
        );
        let states = Expr::attrset(
            false,
            states
                .into_iter()
                .map(|(key, state)| (AttrKey::string(key), nix!(harness.settle(#{state}))))
                .collect(),
        );
        let body = nix!({
            outcomes = #{outcomes};
            states = #{states};
            query = #{query.unwrap_or_else(Expr::null)};
        });

        let mut errors = BTreeMap::new();
        let value = loop {
            match nix.eval(self.binds.clone(), body.clone())? {
                Ok(value) => break value,
                Err(failure) => {
                    let step = failure
                        .aborted_step
                        .filter(|step| !errors.contains_key(step))
                        .context(EvalNixFailedSnafu {
                            stderr: failure.error.clone(),
                        })?;
                    self.binds[self.steps[step].bind].1 = nix!({
                        success = false;
                        value = null;
                    });
                    errors.insert(step, failure.error);
                }
            }
        };
        let results = value["outcomes"].as_array().cloned().unwrap_or_default();

        let unexpected = self.steps.iter().enumerate().position(|(id, step)| {
            step.case.check.wants_success()
                && results.get(id).is_none_or(JsonValue::is_null)
                && !errors.contains_key(&id)
        });
        if let Some(id) = unexpected {
            let step = &self.steps[id];
            let var = Expr::ident(self.binds[step.bind].0.as_str());
            self.binds[step.bind].1 = nix!(harness.unguarded(#{step_id(id)}, #{step.expr.clone()}));
            if let Err(failure) = nix.eval(self.binds.clone(), nix!(#{var}.success))? {
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
        let states = value["states"]
            .as_object()
            .map(|states| states.clone().into_iter().collect())
            .unwrap_or_default();
        Ok(Flushed {
            outcomes,
            states,
            query: value["query"].clone(),
        })
    }
}

fn step_id(index: usize) -> i64 {
    i64::try_from(index).expect("step count fits in i64")
}

/// A step instantiating the module at `path` under `key`; `imports` are the
/// module names it imports from.
pub fn instantiate_expr(
    path: &Path, key: &str, imports: &[String], modules: &BTreeMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) -> Expr {
    new_module_expr(
        path,
        key,
        imports,
        modules,
        aliases,
        nix!({
            results = [];
            st = (m_new.instantiate({})).st;
        }),
    )
}

/// A step producing the pre-initializer state of the module at `path`.
pub fn initial_state_expr(
    path: &Path, key: &str, imports: &[String], modules: &BTreeMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) -> Expr {
    new_module_expr(
        path,
        key,
        imports,
        modules,
        aliases,
        nix!({
            results = [];
            st = m_new."__wasm2nix"."st0";
        }),
    )
}

fn new_module_expr(
    path: &Path, key: &str, imports: &[String], modules: &BTreeMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, body: Expr,
) -> Expr {
    let roots = imports.iter().map(|name| resolve_alias(aliases, name));
    let (mut binds, _) = world(modules, aliases, roots);
    binds.push(("m_new".into(), import_module_expr(path, key)));
    Expr::let_in(binds, body)
}

/// A step invoking export `name` of module `key`.
pub fn invoke_expr(
    key: &str, name: &str, args: &[WastArg<'_>], modules: &BTreeMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) -> Result<Expr> {
    let mut args = args
        .iter()
        .map(wast_arg_to_nix)
        .collect::<Result<Vec<_>>>()?;
    args.push(Expr::ident("st_in"));
    export_expr(key, name, modules, aliases, |export| {
        (
            ("call".into(), Expr::app(export, args)),
            nix!({
                results = call.results;
                st = call.st;
            }),
        )
    })
}

/// A step reading global export `name` of module `key`.
pub fn get_expr(
    key: &str, name: &str, modules: &BTreeMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) -> Result<Expr> {
    export_expr(key, name, modules, aliases, |export| {
        (
            ("value".into(), nix!(#{export}.get(st_in))),
            nix!({
                results = [value];
                st = st_in;
            }),
        )
    })
}

/// Evaluates the body returned by `use_export` with export `name` of module
/// `key` and its state `st_in` in scope; `use_export` also returns one extra
/// binding.
fn export_expr(
    key: &str, name: &str, modules: &BTreeMap<String, HarnessModule>,
    aliases: &HashMap<String, String>, use_export: impl FnOnce(Expr) -> ((String, Expr), Expr),
) -> Result<Expr> {
    let state = modules
        .get(key)
        .context(UnknownModuleSnafu { key })?
        .state
        .clone();
    let (mut binds, vars) = world(modules, aliases, [key.to_string()]);
    let module = Expr::ident(vars[key].as_str());
    let export_key = format!("__wasm2nix_export_{}", hex::encode(name.as_bytes()));
    let (bind, body) = use_export(nix!(#{module}."__wasm2nix"."exportsByName".(export_key)));
    binds.push(("st_in".into(), state));
    binds.push(bind);
    Ok(Expr::let_in(binds, body))
}

/// Bindings importing, with their current states, the modules connected to
/// `roots` by imports in either direction: a step can only reach those, and
/// the function registry needs every module whose functions can land in a
/// shared table. Also returns the variable bound to each module key.
fn world(
    modules: &BTreeMap<String, HarnessModule>, aliases: &HashMap<String, String>,
    roots: impl IntoIterator<Item = String>,
) -> (Vec<(String, Expr)>, HashMap<String, String>) {
    let mut neighbours = HashMap::<&str, Vec<&str>>::new();
    for (key, module) in modules {
        for name in &module.imports {
            if let Some((target, _)) = modules.get_key_value(&resolve_alias(aliases, name)) {
                neighbours.entry(key).or_default().push(target);
                neighbours.entry(target).or_default().push(key);
            }
        }
    }
    let mut component = BTreeSet::new();
    let mut queue = roots
        .into_iter()
        .filter_map(|root| modules.get_key_value(&root).map(|(key, _)| key.as_str()))
        .collect::<Vec<_>>();
    while let Some(key) = queue.pop() {
        if component.insert(key) {
            queue.extend(neighbours.get(key).into_iter().flatten());
        }
    }

    let vars = component
        .iter()
        .enumerate()
        .map(|(i, key)| ((*key).to_string(), format!("m{i}_import")))
        .collect::<HashMap<_, _>>();
    let mut binds = vec![
        ("imports".into(), imports_expr(aliases, &vars)),
        (
            "funcRegistry".into(),
            Expr::attrset(
                false,
                component
                    .iter()
                    .map(|key| {
                        let module = Expr::ident(vars[*key].as_str());
                        (AttrKey::string(*key), nix!(#{module}."__wasm2nix"."funcs"))
                    })
                    .collect(),
            ),
        ),
    ];
    for (i, key) in component.iter().enumerate() {
        let raw = format!("m{i}");
        binds.push((raw.clone(), import_module_expr(&modules[*key].path, key)));
        binds.push((
            vars[*key].clone(),
            nix!(merge(#{Expr::ident(raw)}, {
                "__wasm2nixState" = #{modules[*key].state.clone()};
            })),
        ));
    }
    (binds, vars)
}

/// The import set shared by every module of a world: each module, by key and
/// by registered alias, plus `spectest` and the function registry.
fn imports_expr(aliases: &HashMap<String, String>, vars: &HashMap<String, String>) -> Expr {
    let mut fields = BTreeMap::new();
    fields.insert("__funcRegistry".to_string(), Expr::ident("funcRegistry"));
    fields.insert("spectest".to_string(), nix!(harness.spectest));
    for (key, var) in vars {
        fields.insert(key.clone(), Expr::ident(var.as_str()));
    }
    for (alias, key) in aliases {
        if let Some(var) = vars.get(key) {
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

fn import_expr(path: &Path, arg: Expr) -> Expr {
    nix!(import(#{Expr::path(path.display().to_string())}, #{arg}))
}

/// Imports the module at `path` as `key`, with the world's shared imports.
fn import_module_expr(path: &Path, key: &str) -> Expr {
    nix!(import(#{Expr::path(path.display().to_string())}, rt, merge(imports, {
        "__moduleId" = #{Expr::string(key)};
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
        _ => UnsupportedInvokeArgumentSnafu.fail(),
    }
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
