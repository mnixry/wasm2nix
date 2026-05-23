pub mod clif_to_nix;
pub mod error;
pub mod log;
pub mod nix;
pub mod translator;

pub fn lower_wasm_to_nix(wasm: &[u8]) -> Result<String, String> {
    let module = translator::NixModule::translate(wasm).map_err(|e| e.to_string())?;
    Ok(clif_to_nix::lower_module(&module).render())
}
