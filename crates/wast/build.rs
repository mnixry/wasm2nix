use std::{
    env, fs,
    path::{Path, PathBuf},
};

use proc_macro2::{Ident, Literal, Span, TokenStream};
use quote::quote;
use snafu::prelude::*;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
enum BuildError {
    #[snafu(display("failed to read CARGO_MANIFEST_DIR"))]
    ManifestDir { source: env::VarError },

    #[snafu(display("OUT_DIR must be set"))]
    MissingOutDir,

    #[snafu(display("failed to read WAST spec directory {}", path.display()))]
    ReadSpecDir {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("failed to read WAST spec directory entry in {}", path.display()))]
    ReadSpecEntry {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("failed to write generated WAST tests to {}", path.display()))]
    WriteGenerated {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("WAST file name must be UTF-8: {}", path.display()))]
    NonUtf8WastFileName { path: PathBuf },
}

type Result<T> = std::result::Result<T, BuildError>;

fn main() -> Result<()> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").context(ManifestDirSnafu)?;
    let spec_dir = Path::new(&manifest_dir).join("tests/spec/test/core");

    println!("cargo:rerun-if-changed={}", spec_dir.display());

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").context(MissingOutDirSnafu)?);
    let generated_path = out_dir.join("wast_generated_tests.rs");
    if !spec_dir.is_dir() {
        fs::write(
            &generated_path,
            ignored_missing_spec_test_tokens().to_string(),
        )
        .context(WriteGeneratedSnafu {
            path: generated_path,
        })?;
        return Ok(());
    }

    let mut wast_files = Vec::new();
    for entry in fs::read_dir(&spec_dir).context(ReadSpecDirSnafu {
        path: spec_dir.clone(),
    })? {
        wast_files.push(
            entry
                .context(ReadSpecEntrySnafu {
                    path: spec_dir.clone(),
                })?
                .path(),
        );
    }
    wast_files.retain(|path| path.extension().is_some_and(|ext| ext == "wast"));
    wast_files.sort();

    let generated = wast_tests_tokens(&wast_files)?;

    fs::write(&generated_path, generated.to_string()).context(WriteGeneratedSnafu {
        path: generated_path,
    })?;
    Ok(())
}

fn ignored_missing_spec_test_tokens() -> TokenStream {
    quote! {
        #[test]
        #[ignore = "tests/spec submodule is absent"]
        fn core_spec_submodule_absent() {}
    }
}

fn wast_tests_tokens(wast_files: &[PathBuf]) -> Result<TokenStream> {
    let tests = wast_files
        .iter()
        .map(|path| wast_test_tokens(path))
        .collect::<Result<Vec<_>>>()?;

    Ok(quote! {
        #(#tests)*
    })
}

fn wast_test_tokens(path: &Path) -> Result<TokenStream> {
    println!("cargo:rerun-if-changed={}", path.display());
    let stem =
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .context(NonUtf8WastFileNameSnafu {
                path: path.to_path_buf(),
            })?;
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
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out
        .as_bytes()
        .first()
        .is_none_or(|byte| !byte.is_ascii_alphabetic() && *byte != b'_')
    {
        out.insert(0, '_');
    }
    out
}
