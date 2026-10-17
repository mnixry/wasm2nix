use std::path::PathBuf;

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
    #[snafu(display("{count} WAST case(s) failed:\n{report}"))]
    Failures { count: usize, report: String },

    #[snafu(display("{at}: {source}"))]
    Directive {
        at: String,
        #[snafu(source(from(Error, Box::new)))]
        source: Box<Self>,
    },

    #[snafu(display("failed to create temporary directory"))]
    CreateTempDir { source: std::io::Error },

    #[snafu(display("failed to write {}", path.display()))]
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
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
    LowerNix { source: wasm2nix::lower::LowerError },

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

    #[snafu(display("failed to encode JSON"))]
    EncodeJson { source: serde_json::Error },

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

    #[snafu(display("imported table {module_name}.{field_name} has no table export"))]
    MissingTableExport {
        module_name: String,
        field_name: String,
    },

    #[snafu(display("unsupported imported offset global {module_name}.{field_name}"))]
    UnsupportedImportedOffsetGlobal {
        module_name: String,
        field_name: String,
    },

    #[snafu(display("module instantiation traps during active segment initialization"))]
    ModuleInstantiationTrapped,

    #[snafu(display("assert_return module execution is not supported by this harness"))]
    UnsupportedAssertReturnModuleExecution,

    #[snafu(display("unsupported WAST directive"))]
    UnsupportedWastDirective,

    #[snafu(display("unsupported invoke argument"))]
    UnsupportedInvokeArgument,

    #[snafu(display("unsupported assert_return result"))]
    UnsupportedAssertReturnResult,

    #[snafu(display("expected a non-empty result array"))]
    ExpectedResult,

    #[snafu(display("expected integer result"))]
    ExpectedIntegerResult,

    #[snafu(display("limits query did not return an object"))]
    LimitsQueryNotObject,

    #[snafu(display("Nix integer literal overflow for {value}"))]
    NixIntegerOverflow { value: u64 },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
