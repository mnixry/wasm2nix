use std::fs;
use std::path::Path;
use std::path::PathBuf;

use clap::Parser;
use snafu::ResultExt;
use snafu::Snafu;
use wasm2nix::TranspileOptions;
use wasm2nix::default_output_path;
use wasm2nix::generated_nix_file;
use wasm2nix::linking::ImportMap;
use wasm2nix::linking::{self};
use wasm2nix::lower_module_to_nix;
use wasm2nix::runtime_import_expr;
use wasm2nix::runtime_path_for_output;
use wasm2nix::translator::NixModule;
use wasm2nix_runtime::runtime_nix;

#[derive(Debug, Parser)]
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
        source: wasm2nix::error::TranslateError,
    },

    #[snafu(display("import validation failed"))]
    ValidateImports {
        source: wasm2nix::linking::LinkError,
    },

    #[snafu(display("Wasm lowering failed"))]
    LowerWasm { source: wasm2nix::lower::LowerError },

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
    run(&Args::parse())
}

fn run(args: &Args) -> Result<()> {
    let wasm = fs::read(&args.input).context(ReadWasmSnafu { path: &args.input })?;
    let module = NixModule::translate(&wasm).context(TranslateWasmSnafu)?;
    if let Some(imports_path) = &args.imports {
        validate_import_manifest(&module, imports_path)?;
    }

    let out_path = args
        .output
        .clone()
        .unwrap_or_else(|| default_output_path(&args.input));
    let runtime_path = runtime_path_for_output(&out_path, args.export_rt_path.as_deref());
    let mut options = TranspileOptions::new(runtime_import_expr(
        &out_path,
        args.export_rt_path.as_deref(),
    ));
    if let Some(width) = args.line_width {
        options.render_options.width = width;
    }
    if args.compact {
        options.render_options.style = wasm2nix::nix::RenderStyle::Compact;
    }

    let nix_expr = lower_module_to_nix(&module, &options).context(LowerWasmSnafu)?;
    let nix = generated_nix_file(Some(&args.input), &nix_expr, &options.render_options);

    if !args.no_export_rt {
        fs::write(&runtime_path, runtime_nix()).context(WriteRuntimeSnafu {
            path: &runtime_path,
        })?;
    }
    fs::write(&out_path, nix).context(WriteNixSnafu { path: &out_path })
}

fn validate_import_manifest(module: &NixModule, path: &Path) -> Result<()> {
    let source = fs::read_to_string(path).context(ReadImportsSnafu { path })?;
    let mut imports = parse_imports_manifest(path, &source)?;
    linking::validate_imports(module, &mut imports).context(ValidateImportsSnafu)
}

fn parse_imports_manifest(path: &Path, source: &str) -> Result<ImportMap> {
    serde_json::from_str(source).context(ParseImportsSnafu { path })
}

#[cfg(test)]
mod tests {
    use wasm2nix::linking::FuncType;
    use wasm2nix::linking::GlobalType;
    use wasm2nix::linking::Limits;
    use wasm2nix::linking::TableType;

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
            GlobalType::new("i32".into(), false)
        );
        assert_eq!(
            spectest.tables["table"],
            TableType::new("funcref".into(), Limits::new(10, Some(20)))
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
