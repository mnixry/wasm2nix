use wasm2nix_wast::{run_wast_file_path, run_wast_script};

#[test]
fn wast_harness_i32_assert_return_smoke() {
    let script = r#"
            (module
              (func (export "add") (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.add))
            (assert_return (invoke "add" (i32.const 40) (i32.const 2)) (i32.const 42))
        "#;

    if let Err(err) = run_wast_script(script) {
        panic!("wast smoke should pass: {err}");
    }
}

include!(concat!(env!("OUT_DIR"), "/wast_generated_tests.rs"));
