use wasm2nix_runtime::{RuntimeFunction, RuntimeNamespace};

use super::lower_module;
use crate::{nix::Expr, translator::NixModule};

fn lower_wasm(wasm: &[u8]) -> String {
    let module = match NixModule::translate(wasm) {
        Ok(module) => module,
        Err(err) => panic!("wasm should translate: {err}"),
    };
    match lower_module(&module) {
        Ok(expr) => expr.render(),
        Err(err) => panic!("wasm should lower: {err}"),
    }
}

fn lower_wat(wat: &str) -> String {
    let buf = match wast::parser::ParseBuffer::new(wat) {
        Ok(buf) => buf,
        Err(err) => panic!("wat should parse: {err}"),
    };
    let mut wat = match wast::parser::parse::<wast::Wat<'_>>(&buf) {
        Ok(wat) => wat,
        Err(err) => panic!("wat should parse: {err}"),
    };
    let wasm = match wat.encode() {
        Ok(wasm) => wasm,
        Err(err) => panic!("wat should encode: {err}"),
    };
    lower_wasm(&wasm)
}

fn assert_contains_all<I, S>(haystack: &str, needles: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>, {
    for needle in needles {
        let needle = needle.as_ref();
        assert!(
            haystack.contains(needle),
            "expected generated Nix to contain {needle}"
        );
    }
}

fn runtime_exprs(functions: impl IntoIterator<Item = RuntimeFunction>) -> Vec<String> {
    functions
        .into_iter()
        .map(|function| function.expr().render())
        .collect()
}

#[test]
fn generated_module_inlines_single_use_immediates_into_bindings() {
    let nix = lower_wat(
        r#"
        (module
          (func (export "mul_consts") (result i32)
            i32.const 40
            i32.const 2
            i32.mul))
        "#,
    );

    let expected = RuntimeFunction::I32Mul
        .call(vec![Expr::int(40), Expr::int(2)])
        .render();
    assert!(nix.contains(&expected));
    assert!(
        !nix.contains(" = 40;") && !nix.contains(" = 2;"),
        "single-use immediate should be inlined instead of bound"
    );
}

#[test]
fn generated_module_inlines_cheap_integer_ops() {
    let nix = lower_wat(
        r#"
        (module
          (func (export "f") (param i32 i32) (result i32)
            (i32.shl (i32.add (local.get 0) (local.get 1)) (i32.const 3))))
        "#,
    );

    assert!(nix.contains("builtins.bitAnd (builtins.bitAnd (v0 + v1) 4294967295 * 8) 4294967295"));
    assert!(!nix.contains(&RuntimeFunction::I32Add.expr().render()));
    assert!(!nix.contains(&RuntimeFunction::I32Shl.expr().render()));
}

#[test]
fn generated_module_keeps_compat_exports_and_instantiate() {
    let wasm = [
        0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x01, 0x07, 0x01, 0x60, 0x02, 0x7F, 0x7F,
        0x01, 0x7F, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64, 0x00, 0x00,
        0x0A, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6A, 0x0B,
    ];
    let nix = lower_wasm(&wasm);

    assert!(nix.contains("\"add\" = funcs.f0;"));
    assert!(nix.contains("instantiate = args:"));
    assert!(nix.contains("st = st;"));
    assert!(nix.contains("exports = exports;"));
    assert!(nix.contains("__wasm2nix ="));
    assert!(
        !nix.contains("builtins.genericClosure"),
        "loop-free functions call their blocks directly"
    );
}

#[test]
fn generated_module_trampolines_loop_back_edges() {
    let nix = lower_wat(
        r#"
        (module
          (func (export "count") (param i32) (result i32) (local i32)
            (block
              (loop
                (br_if 1 (i32.eqz (local.get 0)))
                (local.set 1 (i32.add (local.get 1) (i32.const 2)))
                (local.set 0 (i32.sub (local.get 0) (i32.const 1)))
                (br 0)))
            (local.get 1)))
        "#,
    );

    assert!(nix.contains("builtins.genericClosure"));
    assert!(nix.contains("builtins.deepSeq nextKeyed nextKeyed"));
    assert!(nix.contains("tag = \"jump\";"));
    assert!(
        !nix.contains("args = [ ("),
        "loop-carried values stay forced bindings"
    );
}

#[test]
fn generated_module_embeds_active_data_segments() {
    let wasm = [
        0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7F,
        0x03, 0x02, 0x01, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07, 0x11, 0x02, 0x06, 0x6D, 0x65,
        0x6D, 0x6F, 0x72, 0x79, 0x02, 0x00, 0x04, 0x6C, 0x6F, 0x61, 0x64, 0x00, 0x00, 0x0A, 0x09,
        0x01, 0x07, 0x00, 0x41, 0x04, 0x28, 0x02, 0x00, 0x0B, 0x0B, 0x0A, 0x01, 0x00, 0x41, 0x04,
        0x0B, 0x04, 0x01, 0x02, 0x03, 0x04,
    ];
    let nix = lower_wasm(&wasm);

    assert!(nix.contains("pageCount = 1;"));
    assert!(nix.contains("\"0\" = { \"1\" = 67305985; };"));
    assert!(nix.contains(&format!(
        "\"memory\" = {} // {{",
        RuntimeNamespace::Mem.expr().render()
    )));
}

#[test]
fn generated_module_runs_start_before_exposing_instance_state() {
    let wasm = [
        0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03,
        0x02, 0x01, 0x00, 0x06, 0x06, 0x01, 0x7F, 0x01, 0x41, 0x00, 0x0B, 0x07, 0x05, 0x01, 0x01,
        0x67, 0x03, 0x00, 0x08, 0x01, 0x00, 0x0A, 0x08, 0x01, 0x06, 0x00, 0x41, 0x2A, 0x24, 0x00,
        0x0B,
    ];
    let nix = lower_wasm(&wasm);

    assert!(nix.contains("g0 = 0;"));
    assert!(nix.contains("\"0\" = g0;"));
    assert!(nix.contains("st = (funcs.f0 st0).st;"));
    assert!(nix.contains(&RuntimeFunction::GlobalSetI32.expr().render()));
    assert!(nix.contains("\"g\" = {"));
}

#[test]
fn generated_module_initializes_imported_global_get_and_ref_null_globals() {
    let nix = lower_wat(
        r#"
        (module
          (global (import "spectest" "global_i32") i32)
          (global $copy i32 (global.get 0))
          (global $r externref (ref.null extern))
          (func (export "copy") (result i32) (global.get $copy))
          (func (export "ref") (result externref) (global.get $r)))
        "#,
    );

    assert!(nix.contains("g0 = if"));
    assert!(nix.contains("builtins.isAttrs imports.\"spectest\".\"global_i32\""));
    assert!(nix.contains("g1 = g0;"));
    assert!(nix.contains("g2 = 0;"));
    assert!(nix.contains("\"0\" = g0;"));
    assert!(nix.contains("\"1\" = g1;"));
    assert!(nix.contains("\"2\" = g2;"));
}

#[test]
fn generated_module_lowers_memory_intrinsics_to_runtime_calls() {
    let nix = lower_wat(
        r#"
        (module
          (memory 1)
          (data $d "abc")
          (func (export "m")
            (drop (memory.size))
            (drop (memory.grow (i32.const 0)))
            (memory.copy (i32.const 0) (i32.const 1) (i32.const 1))
            (memory.fill (i32.const 0) (i32.const 0) (i32.const 1))
            (memory.init $d (i32.const 0) (i32.const 0) (i32.const 1))
            (data.drop $d)))
        "#,
    );

    assert_contains_all(
        &nix,
        runtime_exprs([
            RuntimeFunction::MemSize,
            RuntimeFunction::MemGrow,
            RuntimeFunction::MemCopy,
            RuntimeFunction::MemFill,
            RuntimeFunction::MemInit,
            RuntimeFunction::MemDataDrop,
        ]),
    );
}

#[test]
fn generated_module_lowers_table_intrinsics_to_runtime_calls() {
    let nix = lower_wat(
        r#"
        (module
          (table $t 2 funcref)
          (elem $e funcref (ref.null func))
          (func (export "t")
            (drop (table.size $t))
            (drop (table.grow $t (ref.null func) (i32.const 0)))
            (drop (table.get $t (i32.const 0)))
            (table.set $t (i32.const 0) (ref.null func))
            (table.fill $t (i32.const 0) (ref.null func) (i32.const 1))
            (table.copy $t $t (i32.const 0) (i32.const 1) (i32.const 1))
            (table.init $t $e (i32.const 0) (i32.const 0) (i32.const 1))
            (elem.drop $e)))
        "#,
    );

    assert_contains_all(
        &nix,
        runtime_exprs([
            RuntimeFunction::TableSize,
            RuntimeFunction::TableGrow,
            RuntimeFunction::TableGet,
            RuntimeFunction::TableSet,
            RuntimeFunction::TableFill,
            RuntimeFunction::TableCopy,
            RuntimeFunction::TableInit,
            RuntimeFunction::TableElemDrop,
        ]),
    );
}

#[test]
fn generated_module_lowers_typed_global_intrinsics_to_runtime_calls() {
    let nix = lower_wat(
        r#"
        (module
          (global $i32 (mut i32) (i32.const 0))
          (global $i64 (mut i64) (i64.const 0))
          (global $f32 (mut f32) (f32.const 0))
          (global $f64 (mut f64) (f64.const 0))
          (func (export "g")
            (global.set $i32 (global.get $i32))
            (global.set $i64 (global.get $i64))
            (global.set $f32 (global.get $f32))
            (global.set $f64 (global.get $f64))))
        "#,
    );

    assert_contains_all(
        &nix,
        runtime_exprs([
            RuntimeFunction::GlobalGetI32,
            RuntimeFunction::GlobalGetI64,
            RuntimeFunction::GlobalGetF32,
            RuntimeFunction::GlobalGetF64,
            RuntimeFunction::GlobalSetI32,
            RuntimeFunction::GlobalSetI64,
            RuntimeFunction::GlobalSetF32,
            RuntimeFunction::GlobalSetF64,
        ]),
    );
}

#[test]
fn generated_module_lowers_call_indirect_through_table_runtime() {
    let wasm = [
        0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7F, 0x01,
        0x7F, 0x03, 0x03, 0x02, 0x00, 0x00, 0x04, 0x04, 0x01, 0x70, 0x00, 0x01, 0x07, 0x08, 0x01,
        0x04, 0x63, 0x61, 0x6C, 0x6C, 0x00, 0x01, 0x09, 0x07, 0x01, 0x00, 0x41, 0x00, 0x0B, 0x01,
        0x00, 0x0A, 0x13, 0x02, 0x07, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6A, 0x0B, 0x09, 0x00, 0x20,
        0x00, 0x41, 0x00, 0x11, 0x00, 0x00, 0x0B,
    ];
    let nix = lower_wasm(&wasm);

    assert!(nix.contains(&format!(
        "{} 0 \"(i32)->(i32)\" moduleId funcs funcRegistry",
        RuntimeFunction::TableCall.expr().render()
    )));
    assert!(nix.contains("tables = {"));
    assert!(nix.contains("module = moduleId;"));
    assert!(nix.contains("func = 0;"));
    assert!(nix.contains("type = \"(i32)->(i32)\";"));
}
