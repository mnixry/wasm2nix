use snafu::Snafu;

#[derive(Debug, Snafu)]
#[snafu(display("WAST harness resolver failed"))]
pub struct HarnessResolverError {
    #[snafu(source(from(Error, Box::new)))]
    source: Box<Error>,
}

impl From<Error> for HarnessResolverError {
    fn from(source: Error) -> Self {
        Self {
            source: Box::new(source),
        }
    }
}

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    #[snafu(display("failed to create temporary directory"))]
    CreateTempDir { source: std::io::Error },

    #[snafu(display("failed to create temporary {kind} Nix file"))]
    CreateTempFile {
        kind: String,
        source: std::io::Error,
    },

    #[snafu(display("failed to write temporary {kind} Nix file"))]
    WriteTempFile {
        kind: String,
        source: std::io::Error,
    },

    #[snafu(display("failed to keep temporary {kind} Nix file"))]
    KeepTempFile {
        kind: String,
        source: tempfile::PersistError,
    },

    #[snafu(display("failed to parse WAST script"))]
    ParseWast { source: wast::Error },

    #[snafu(display("failed to encode WAST module"))]
    EncodeWat { source: wast::Error },

    #[snafu(display("Wasm translation failed"))]
    TranslateWasm {
        #[snafu(source(from(
            wasm2nix::error::TranslateWasmRawSource,
            wasm2nix::error::TranslateWasmSource::from
        )))]
        source: wasm2nix::error::TranslateWasmSource,
    },

    #[snafu(display("Nix lowering failed"))]
    LowerNix {
        source: wasm2nix::lowerings::LowerError,
    },

    #[snafu(display("link validation failed"))]
    Link {
        source: wasm2nix::linking::LinkError<HarnessResolverError>,
    },

    #[snafu(display("active initializer analysis failed"))]
    ActiveInitializer {
        source: wasm2nix::instantiation::InstantiationError<HarnessResolverError>,
    },

    #[snafu(display("failed to spawn nix eval"))]
    EvalNix { source: std::io::Error },

    #[snafu(display("nix eval failed: {stderr}"))]
    EvalNixFailed { stderr: String },

    #[snafu(display("failed to decode nix eval JSON"))]
    DecodeEvalJson { source: serde_json::Error },

    #[snafu(display("failed to encode JSON as Nix expression"))]
    EncodeJson { source: serde_json::Error },

    #[snafu(display("directive {directive_index}: {source}"))]
    Directive {
        directive_index: usize,
        source: Box<Self>,
    },

    #[snafu(display("directive {directive_index}: invoke {name} failed: {source}"))]
    Invoke {
        directive_index: usize,
        name: String,
        source: Box<Self>,
    },

    #[snafu(display("module access before module"))]
    ModuleAccessBeforeModule,

    #[snafu(display("unknown module {key}"))]
    UnknownModule { key: String },

    #[snafu(display("unknown import module {module_name}"))]
    UnknownImportModule { module_name: String },

    #[snafu(display("unknown import {module_name}.{field_name}"))]
    UnknownImport {
        module_name: String,
        field_name: String,
    },

    #[snafu(display("incompatible import type {module_name}.{field_name}"))]
    IncompatibleImportType {
        module_name: String,
        field_name: String,
    },

    #[snafu(display("missing table limits for {module_name}.{field_name}"))]
    MissingTableLimits {
        module_name: String,
        field_name: String,
    },

    #[snafu(display("missing memory limits for {module_name}.{field_name}"))]
    MissingMemoryLimits {
        module_name: String,
        field_name: String,
    },

    #[snafu(display("unsupported imported offset global {module_name}.{field_name}"))]
    UnsupportedImportedOffsetGlobal {
        module_name: String,
        field_name: String,
    },

    #[snafu(display("module instantiation trapped during active segment initialization"))]
    ModuleInstantiationTrapped,

    #[snafu(display("assert_trap module instantiated successfully"))]
    TrapModuleInstantiated,

    #[snafu(display("assert_return module execution is not supported by this harness slice"))]
    UnsupportedAssertReturnModuleExecution,

    #[snafu(display("unsupported WAST directive"))]
    UnsupportedWastDirective,

    #[snafu(display("assert_return mismatch: expected {expected}, got {actual}"))]
    AssertReturnMismatch {
        expected: String,
        actual: serde_json::Value,
    },

    #[snafu(display("assert_trap invoke {name} did not trap"))]
    InvokeDidNotTrap { name: String },

    #[snafu(display("assert_trap get {global} did not trap"))]
    GetDidNotTrap { global: String },

    #[snafu(display("{assertion} module was accepted"))]
    ModuleAccepted { assertion: &'static str },

    #[snafu(display("assert_invalid module translated successfully"))]
    InvalidModuleTranslated,

    #[snafu(display("assert_unlinkable module instantiated successfully"))]
    UnlinkableModuleInstantiated,

    #[snafu(display("assert_exhaustion invoke {name} did not fail"))]
    ExhaustionDidNotFail { name: String },

    #[snafu(display("unsupported invoke argument for this harness slice"))]
    UnsupportedInvokeArgument,

    #[snafu(display("unsupported assert_return result for this harness slice"))]
    UnsupportedAssertReturnResult,

    #[snafu(display("NaN patterns must be matched by predicate"))]
    NanPatternRequiresPredicate,

    #[snafu(display("actual results must be a JSON array"))]
    ActualResultsNotArray,

    #[snafu(display("expected result array"))]
    ExpectedResultArray,

    #[snafu(display("expected at least one result"))]
    ExpectedAtLeastOneResult,

    #[snafu(display("expected numeric result"))]
    ExpectedNumericResult,

    #[snafu(display("expected integer result"))]
    ExpectedIntegerResult,

    #[snafu(display("limits query did not return an object"))]
    LimitsQueryNotObject,

    #[snafu(display("Nix integer literal overflow for {value}"))]
    NixIntegerOverflow { value: u64 },

    #[snafu(display("target module {key} has no instantiated state"))]
    TargetMissingState { key: String },

    #[snafu(display("source module {key} has no instantiated state"))]
    SourceMissingState { key: String },

    #[snafu(display("imported table {module_name}.{field_name} has no table export"))]
    MissingTableExport {
        module_name: String,
        field_name: String,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn directive<T>(directive_index: usize, result: Result<T>) -> Result<T> {
    result.map_err(|source| Error::Directive {
        directive_index,
        source: Box::new(source),
    })
}

pub(crate) fn invoke<T>(directive_index: usize, name: &str, result: Result<T>) -> Result<T> {
    result.map_err(|source| Error::Invoke {
        directive_index,
        name: name.to_string(),
        source: Box::new(source),
    })
}
