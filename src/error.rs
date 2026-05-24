use std::path::PathBuf;

use snafu::prelude::*;

#[derive(Debug)]
pub struct TranslateWasmSource(String);

impl From<cranelift_wasm::WasmError> for TranslateWasmSource {
    fn from(source: cranelift_wasm::WasmError) -> Self {
        Self(source.to_string())
    }
}

impl std::fmt::Display for TranslateWasmSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TranslateWasmSource {}

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(display("Failed to read Wasm input from {}", path.display()))]
    ReadWasm {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("Failed to write Nix output to {}", path.display()))]
    WriteNix {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("Wasm translation failed"))]
    TranslateWasm { source: TranslateWasmSource },

    #[snafu(display("Nix lowering failed: {source}"))]
    LowerNix {
        source: crate::lowerings::LowerError,
    },

    #[snafu(display("{message}"))]
    Message { message: String },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
