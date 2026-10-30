//! Generates one test per spec WAST file, embedding the script with
//! `include_str!` so the test binary does not read the spec tree at run time.

use std::env;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use proc_macro2::Ident;
use proc_macro2::Span;
use proc_macro2::TokenStream;
use quote::quote;
use snafu::OptionExt;
use snafu::ResultExt;
use snafu::Snafu;

#[derive(Debug, Snafu)]
enum Error {
    #[snafu(display("failed to list {}", path.display()))]
    ListSpec { path: PathBuf, source: io::Error },

    #[snafu(display("WAST file name is not UTF-8: {}", path.display()))]
    NonUtf8Name { path: PathBuf },

    #[snafu(display("OUT_DIR must be set"))]
    MissingOutDir,

    #[snafu(display("failed to write {}", path.display()))]
    Write { path: PathBuf, source: io::Error },
}

fn main() -> Result<(), Error> {
    let spec_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/spec/test/core");
    // Cargo rescans a directory for changed files.
    println!("cargo:rerun-if-changed={}", spec_dir.display());

    let generated = if spec_dir.is_dir() {
        let mut wast_files = fs::read_dir(&spec_dir)
            .and_then(|entries| {
                entries
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<io::Result<Vec<_>>>()
            })
            .context(ListSpecSnafu { path: &spec_dir })?;
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

    let out_path = PathBuf::from(env::var_os("OUT_DIR").context(MissingOutDirSnafu)?)
        .join("wast_generated_tests.rs");
    fs::write(&out_path, generated.to_string()).context(WriteSnafu { path: out_path })
}

/// `core_<name>`, running `<name>.wast`.
fn wast_test_tokens(path: &Path) -> Result<TokenStream, Error> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context(NonUtf8NameSnafu { path })?;
    let name = stem
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    let test_name = Ident::new(&format!("core_{name}"), Span::call_site());
    let path = path.to_string_lossy();

    Ok(quote! {
        #[test]
        #[snafu::report]
        fn #test_name() -> ::wasm2nix_wast::Result<()> {
            ::wasm2nix_wast::run_wast_script(include_str!(#path))
        }
    })
}
