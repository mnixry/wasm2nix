//! Generates one test per spec WAST file, embedding the script with
//! `include_str!` so the test binary does not read the spec tree at run time.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use proc_macro2::{Ident, Literal, Span, TokenStream};
use quote::quote;

fn main() -> Result<(), Box<dyn Error>> {
    let spec_dir = Path::new(&env::var("CARGO_MANIFEST_DIR")?).join("tests/spec/test/core");
    println!("cargo:rerun-if-changed={}", spec_dir.display());

    let generated = if spec_dir.is_dir() {
        let mut wast_files = fs::read_dir(&spec_dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        wast_files.retain(|path| path.extension().is_some_and(|ext| ext == "wast"));
        wast_files.sort();
        wast_files
            .iter()
            .map(|path| wast_test_tokens(path))
            .collect::<Result<TokenStream, _>>()?
    } else {
        quote! {
            #[test]
            #[ignore = "tests/spec submodule is absent"]
            fn core_spec_submodule_absent() {}
        }
    };

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR must be set")?);
    fs::write(
        out_dir.join("wast_generated_tests.rs"),
        generated.to_string(),
    )?;
    Ok(())
}

fn wast_test_tokens(path: &Path) -> Result<TokenStream, Box<dyn Error>> {
    println!("cargo:rerun-if-changed={}", path.display());
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("WAST file name must be UTF-8: {}", path.display()))?;
    let test_name = Ident::new(&format!("core_{}", sanitize_ident(stem)), Span::call_site());
    let path = Literal::string(path.to_string_lossy().as_ref());

    Ok(quote! {
        #[test]
        #[snafu::report]
        fn #test_name() -> Result<()> {
            run_wast_script(include_str!(#path))
        }
    })
}

fn sanitize_ident(input: &str) -> String {
    let mut out = input
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    if out
        .as_bytes()
        .first()
        .is_none_or(|byte| !byte.is_ascii_alphabetic() && *byte != b'_')
    {
        out.insert(0, '_');
    }
    out
}
