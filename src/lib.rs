pub mod clif_to_nix;
pub mod error;
pub mod instantiation;
pub mod intrinsics;
pub mod lowerings;
pub mod nix;
pub mod translator;

use snafu::prelude::*;

pub fn lower_wasm_to_nix(wasm: &[u8]) -> error::Result<String> {
    let module = translator::NixModule::translate(wasm)
        .map_err(error::TranslateWasmSource::from)
        .context(error::TranslateWasmSnafu)?;
    let lowered = lowerings::lower_module(&module).context(error::LowerNixSnafu)?;
    Ok(lowered.render())
}
