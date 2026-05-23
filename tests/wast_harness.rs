use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use wast::core::{WastArgCore, WastRetCore};
use wast::parser::{self, ParseBuffer};
use wast::{Wast, WastArg, WastDirective, WastExecute, WastRet};

static NEXT_FILE: AtomicUsize = AtomicUsize::new(0);

#[test]
#[ignore = "requires nix eval and currently covers the harness smoke path only"]
fn wast_harness_i32_assert_return_smoke() {
    let script = r#"
        (module
          (func (export "add") (param i32 i32) (result i32)
            local.get 0
            local.get 1
            i32.add))
        (assert_return (invoke "add" (i32.const 40) (i32.const 2)) (i32.const 42))
    "#;

    run_wast_script(script).expect("wast smoke should pass");
}

#[test]
#[ignore = "set WASM_SPEC_TEST to a .wast path and run with --ignored"]
fn wast_harness_single_file_from_env() {
    let path = std::env::var("WASM_SPEC_TEST").expect("WASM_SPEC_TEST must point at a .wast file");
    let script = fs::read_to_string(&path).expect("failed to read WASM_SPEC_TEST");
    run_wast_script(&script).expect("wast file should pass supported assertions");
}

fn run_wast_script(script: &str) -> Result<(), String> {
    let buf = ParseBuffer::new(script).map_err(|e| e.to_string())?;
    let wast = parser::parse::<Wast<'_>>(&buf).map_err(|e| e.to_string())?;
    let mut current_module: Option<PathBuf> = None;

    for directive in wast.directives {
        match directive {
            WastDirective::Module(mut module) => {
                let wasm = module.encode().map_err(|e| e.to_string())?;
                let nix = wasm2nix::lower_wasm_to_nix(&wasm)?;
                let path = write_generated_module(&nix)?;
                current_module = Some(path);
            }
            WastDirective::AssertReturn { exec, results, .. } => {
                let module = current_module
                    .as_ref()
                    .ok_or_else(|| "assert_return before module".to_string())?;
                let WastExecute::Invoke(invoke) = exec else {
                    return Err(
                        "only assert_return invoke is supported by this harness slice".into(),
                    );
                };
                let actual = eval_invoke(module, invoke.name, &invoke.args)?;
                let expected = expected_results_json(&results)?;
                if actual != expected {
                    return Err(format!(
                        "assert_return mismatch for {}: expected {expected}, got {actual}",
                        invoke.name
                    ));
                }
            }
            WastDirective::Invoke(invoke) => {
                let module = current_module
                    .as_ref()
                    .ok_or_else(|| "invoke before module".to_string())?;
                let _ = eval_invoke(module, invoke.name, &invoke.args)?;
            }
            _ => {}
        }
    }

    Ok(())
}

fn write_generated_module(nix: &str) -> Result<PathBuf, String> {
    let id = NEXT_FILE.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("wasm2nix-wast-{id}.nix"));
    fs::write(&path, nix).map_err(|e| e.to_string())?;
    Ok(path)
}

fn eval_invoke(module: &PathBuf, name: &str, args: &[WastArg<'_>]) -> Result<String, String> {
    let args = args
        .iter()
        .map(wast_arg_to_nix)
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    let invoke = if args.is_empty() {
        format!("i.exports.\"{name}\" i.st")
    } else {
        format!("i.exports.\"{name}\" {args} i.st")
    };
    let expr = format!(
        "let rt = import ./nix/rt.nix {{}}; m = import {} rt {{}}; i = m.instantiate {{}}; in ({invoke}).results",
        module.display()
    );
    let output = Command::new("nix")
        .args(["eval", "--json", "--impure", "--expr", &expr])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn wast_arg_to_nix(arg: &WastArg<'_>) -> Result<String, String> {
    match arg {
        WastArg::Core(WastArgCore::I32(value)) => Ok((*value as u32).to_string()),
        WastArg::Core(WastArgCore::I64(value)) => Ok(nix_int_literal(*value)),
        _ => Err("only i32/i64 invoke arguments are supported by this harness slice".into()),
    }
}

fn nix_int_literal(value: i64) -> String {
    if value == i64::MIN {
        "(0 - 9223372036854775807 - 1)".to_string()
    } else if value < 0 {
        format!("({value})")
    } else {
        value.to_string()
    }
}

fn expected_results_json(results: &[WastRet<'_>]) -> Result<String, String> {
    let values = results
        .iter()
        .map(expected_result_to_json)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!("[{}]", values.join(",")))
}

fn expected_result_to_json(ret: &WastRet<'_>) -> Result<String, String> {
    match ret {
        WastRet::Core(WastRetCore::I32(value)) => Ok((*value as u32).to_string()),
        WastRet::Core(WastRetCore::I64(value)) => Ok(value.to_string()),
        _ => Err("only i32/i64 assert_return results are supported by this harness slice".into()),
    }
}
