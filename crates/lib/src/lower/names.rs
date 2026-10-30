//! Names of the Nix bindings the lowering generates.

use cranelift_codegen::ir;
use cranelift_codegen::ir::Block;
use cranelift_wasm::FuncIndex;
use cranelift_wasm::GlobalIndex;

/// The state a function or block starts from.
pub(super) const ST: &str = "st";

pub(super) fn value_name(v: ir::Value) -> String {
    format!("v{}", v.as_u32())
}

pub(super) fn block_name(block: Block) -> String {
    format!("b{}", block.as_u32())
}

pub(super) fn func_name(func: FuncIndex) -> String {
    format!("f{}", func.as_u32())
}

pub(super) fn global_bind_name(global: GlobalIndex) -> String {
    format!("g{}", global.as_u32())
}
