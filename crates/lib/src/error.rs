pub use cranelift_wasm::WasmError as TranslateWasmRawSource;
use snafu::prelude::*;

#[derive(Debug)]
pub struct TranslateWasmSource(TranslateWasmRawSource);

impl From<TranslateWasmRawSource> for TranslateWasmSource {
    fn from(source: TranslateWasmRawSource) -> Self {
        Self(source)
    }
}

impl std::fmt::Display for TranslateWasmSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for TranslateWasmSource {}

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(display("Wasm translation failed"))]
    TranslateWasm {
        #[snafu(source(from(TranslateWasmRawSource, TranslateWasmSource::from)))]
        source: TranslateWasmSource,
    },

    #[snafu(display("Nix lowering failed"))]
    LowerNix {
        source: crate::lowerings::LowerError,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
