pub mod error;
pub mod instantiation;
pub mod intrinsics;
pub mod lowerings;
pub mod translator;

use std::path::Path;

use snafu::prelude::*;
pub use wasm2nix_ast as nix;

pub const DEFAULT_RUNTIME_FILE_NAME: &str = "wasm2nix-rt.nix";

pub fn runtime_nix() -> &'static str {
    include_str!("../nix/rt.nix")
}

pub fn runtime_nix_path() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/nix/rt.nix"))
}

pub fn lower_wasm_to_nix(wasm: &[u8], runtime_path_expr: &str) -> error::Result<String> {
    let module = lower_wasm_to_nix_module(wasm)?;
    Ok(format!(
        "{{ imports ? {{ }}, rt ? import {runtime_path_expr} {{ }} }}:\n\n({module}) rt imports"
    ))
}

pub fn lower_wasm_to_nix_module(wasm: &[u8]) -> error::Result<String> {
    let module = translator::NixModule::translate(wasm)
        .map_err(error::TranslateWasmSource::from)
        .context(error::TranslateWasmSnafu)?;
    let lowered = lowerings::lower_module(&module).context(error::LowerNixSnafu)?;
    Ok(lowered.render())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_nix_imports_default_runtime() {
        let wasm = [
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00, 0x0A, 0x04, 0x01, 0x02, 0x00, 0x0B,
        ];

        let nix = lower_wasm_to_nix(&wasm, &format!("./{DEFAULT_RUNTIME_FILE_NAME}"))
            .expect("wasm should lower");

        assert!(nix.starts_with("{ imports ? { }, rt ? import ./wasm2nix-rt.nix { } }:"));
        assert!(nix.ends_with(") rt imports"));
    }

    #[test]
    fn runtime_source_is_library_owned() {
        assert!(runtime_nix().contains("wasm2nix runtime"));
        assert!(runtime_nix_path().ends_with("nix/rt.nix"));
    }
}
