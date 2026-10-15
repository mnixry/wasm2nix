use std::path::{Path, PathBuf};

use clap::Parser;
use snafu::prelude::*;
use wasm2nix::{
    TranspileOptions, default_output_path, generated_nix_file,
    linking::{self, ImportMap},
    lower_wasm_to_nix, runtime_import_expr, runtime_path_for_output,
    translator::NixModule,
};
use wasm2nix_runtime::runtime_nix;

#[derive(Debug, Clone, Parser)]
struct Args {
    /// Input `.wasm` file path.
    input: PathBuf,

    /// Output `.nix` file path (defaults to input path with `.nix` extension).
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Do not write the runtime file next to the generated Nix file.
    #[arg(long)]
    no_export_rt: bool,

    /// Runtime file path to write and import from (relative paths are resolved
    /// from the output file directory).
    #[arg(long, value_name = "PATH")]
    export_rt_path: Option<PathBuf>,

    /// Validate module imports against a JSON import manifest before writing
    /// output.
    #[arg(long, value_name = "PATH")]
    imports: Option<PathBuf>,

    /// Pretty-print line width for generated Nix.
    #[arg(long, value_name = "COLUMNS", conflicts_with = "compact")]
    line_width: Option<usize>,

    /// Emit compact generated Nix without generated comments.
    #[arg(long, conflicts_with = "line_width")]
    compact: bool,
}

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
enum CliError {
    #[snafu(display("failed to read Wasm input from {}", path.display()))]
    ReadWasm {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("failed to read import manifest from {}", path.display()))]
    ReadImports {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("failed to parse import manifest {}", path.display()))]
    ParseImports {
        path: PathBuf,
        source: serde_json::Error,
    },

    #[snafu(display("Wasm translation failed"))]
    TranslateWasm {
        #[snafu(source(from(
            wasm2nix::error::TranslateWasmRawSource,
            wasm2nix::error::TranslateWasmSource::from
        )))]
        source: wasm2nix::error::TranslateWasmSource,
    },

    #[snafu(display("import validation failed"))]
    ValidateImports {
        source: wasm2nix::linking::LinkError,
    },

    #[snafu(display("Wasm lowering failed"))]
    LowerWasm { source: wasm2nix::error::Error },

    #[snafu(display("failed to write runtime file to {}", path.display()))]
    WriteRuntime {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("failed to write Nix output to {}", path.display()))]
    WriteNix {
        path: PathBuf,
        source: std::io::Error,
    },
}

type Result<T, E = CliError> = std::result::Result<T, E>;

#[snafu::report]
fn main() -> Result<()> {
    let args = Args::parse();

    let wasm = std::fs::read(&args.input).context(ReadWasmSnafu {
        path: args.input.clone(),
    })?;

    let out_path = args
        .output
        .clone()
        .unwrap_or_else(|| default_output_path(&args.input));

    let runtime_path = runtime_path_for_output(&out_path, args.export_rt_path.as_deref());
    let runtime_import =
        runtime_import_expr(&out_path, args.export_rt_path.as_deref(), &runtime_path);

    let mut options = TranspileOptions::new(runtime_import);
    if let Some(width) = args.line_width {
        options.render_options.width = width;
    }
    if args.compact {
        options.render_options.style = wasm2nix::nix::RenderStyle::Compact;
    }

    if let Some(imports_path) = args.imports.as_deref() {
        validate_import_manifest(&wasm, imports_path)?;
    }

    let nix_expr = lower_wasm_to_nix(&wasm, &options).context(LowerWasmSnafu)?;
    let nix = generated_nix_file(Some(&args.input), &nix_expr, &options.render_options);

    if !args.no_export_rt {
        std::fs::write(&runtime_path, runtime_nix())
            .context(WriteRuntimeSnafu { path: runtime_path })?;
    }

    std::fs::write(&out_path, nix).context(WriteNixSnafu { path: out_path })?;
    Ok(())
}

fn validate_import_manifest(wasm: &[u8], path: &Path) -> Result<()> {
    let source = std::fs::read_to_string(path).context(ReadImportsSnafu {
        path: path.to_path_buf(),
    })?;
    let mut imports = parse_imports_manifest(path, &source)?;
    let module = NixModule::translate(wasm).context(TranslateWasmSnafu)?;
    linking::validate_imports(&module, &mut imports).context(ValidateImportsSnafu)
}

fn parse_imports_manifest(path: &Path, source: &str) -> Result<ImportMap> {
    serde_json::from_str(source).context(ParseImportsSnafu {
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use wasm2nix::linking::{FuncType, GlobalType, Limits, TableType};

    use super::*;

    #[test]
    fn compact_conflicts_with_line_width() {
        let err =
            Args::try_parse_from(["wasm2nix", "input.wasm", "--compact", "--line-width", "80"])
                .expect_err("compact should conflict with line width");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn parses_line_width_and_import_manifest_path() {
        let args = Args::try_parse_from([
            "wasm2nix",
            "input.wasm",
            "--line-width",
            "72",
            "--imports",
            "imports.json",
        ])
        .expect("args should parse");
        assert_eq!(args.line_width, Some(72));
        assert_eq!(args.imports, Some(PathBuf::from("imports.json")));
    }

    #[test]
    fn parses_import_manifest() {
        let manifest = r#"
            {
              "spectest": {
                "funcs": { "print_i32": { "params": ["i32"], "results": [] } },
                "globals": { "global_i32": { "type": "i32", "mutable": false } },
                "memories": { "memory": { "min": 1, "max": 2 } },
                "tables": { "table": { "type": "funcref", "min": 10, "max": 20 } }
              }
            }
        "#;

        let imports =
            parse_imports_manifest(Path::new("imports.json"), manifest).expect("manifest parses");
        let spectest = &imports.modules["spectest"];
        assert_eq!(
            spectest.funcs["print_i32"],
            FuncType::new(vec!["i32".to_string()], Vec::new())
        );
        assert_eq!(spectest.memories["memory"], Limits::new(1, Some(2)));
        assert_eq!(
            spectest.globals["global_i32"],
            GlobalType::new("i32", false)
        );
        assert_eq!(
            spectest.tables["table"],
            TableType::new("funcref", Limits::new(10, Some(20)))
        );
    }

    #[test]
    fn import_manifest_rejects_unknown_fields() {
        let manifest = r#"
            {
              "spectest": {
                "funcs": {
                  "print_i32": {
                    "params": ["i32"],
                    "results": [],
                    "result": ["i32"]
                  }
                }
              }
            }
        "#;

        let err = parse_imports_manifest(Path::new("imports.json"), manifest)
            .expect_err("unknown fields should be rejected");
        assert!(matches!(err, CliError::ParseImports { .. }));

        let table = r#"{ "m": { "tables": { "t": { "type": "funcref", "min": 1, "size": 2 } } } }"#;
        let err = parse_imports_manifest(Path::new("imports.json"), table)
            .expect_err("unknown table fields should be rejected");
        assert!(matches!(err, CliError::ParseImports { .. }));
    }
}
