use std::fmt;

use cranelift_wasm::WasmError;
use snafu::Snafu;

/// [`WasmError`] as an error source: it only implements `std::error::Error`
/// with `wasmtime-types/std`, which nothing enables.
#[derive(Debug)]
pub struct WasmErrorSource(WasmError);

impl fmt::Display for WasmErrorSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for WasmErrorSource {}

/// The Wasm bytes are malformed, invalid, or use unsupported features.
#[derive(Debug, Snafu)]
#[snafu(display("Wasm translation failed"), visibility(pub(crate)))]
pub struct TranslateError {
    #[snafu(source(from(WasmError, WasmErrorSource)))]
    source: WasmErrorSource,
}

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    #[snafu(display("Wasm translation failed"))]
    TranslateWasm { source: TranslateError },

    #[snafu(display("Nix lowering failed"))]
    LowerNix { source: crate::lower::LowerError },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
