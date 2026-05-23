pub mod clif_to_nix;
pub mod error;
pub mod intrinsics;
pub mod log;
pub mod nix;
pub mod translator;

#[derive(Debug)]
pub struct TranslateStage(&'static str);

impl std::fmt::Display for TranslateStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for TranslateStage {}

#[derive(Debug)]
pub struct TranslateMessage(String);

impl std::fmt::Display for TranslateMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TranslateMessage {}

#[fastrace::trace]
pub fn lower_wasm_to_nix_exn(wasm: &[u8]) -> exn::Result<String, TranslateStage> {
    use exn::ResultExt;

    let module = translator::NixModule::translate(wasm)
        .map_err(|err| TranslateMessage(err.to_string()))
        .or_raise(|| TranslateStage("translate WebAssembly to Cranelift IR"))?;
    Ok(clif_to_nix::lower_module(&module).render())
}

#[fastrace::trace]
pub fn lower_wasm_to_nix(wasm: &[u8]) -> Result<String, String> {
    lower_wasm_to_nix_exn(wasm).map_err(|err| format!("{err:?}"))
}
