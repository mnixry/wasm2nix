pub mod intrinsics;

use wasm2nix_ast::AttrKey;
use wasm2nix_ast::Expr;

pub const DEFAULT_RUNTIME_FILE_NAME: &str = "wasm2nix-rt.nix";

/// Number of u32 words per memory chunk; must match `chunkWords` in `rt.nix`.
pub const MEMORY_CHUNK_WORDS: u64 = 128;

/// Number of chunks per memory group; must match `groupChunks` in `rt.nix`.
pub const MEMORY_GROUP_CHUNKS: u64 = 32;

#[must_use]
pub const fn runtime_nix() -> &'static str {
    include_str!("../nix/rt.nix")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeResultKind {
    Value,
    State,
    Record,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeNamespace {
    Mem,
    Table,
}

impl RuntimeNamespace {
    /// `rt.<namespace>`.
    #[must_use]
    pub fn expr(self) -> Expr {
        rt_path(match self {
            Self::Mem => &["mem"],
            Self::Table => &["table"],
        })
    }
}

/// `rt.<path>`.
fn rt_path(path: &[&str]) -> Expr {
    path.iter().fold(Expr::ident("rt"), |expr, segment| {
        Expr::select(expr, AttrKey::ident(*segment))
    })
}

include!(concat!(env!("OUT_DIR"), "/runtime_calls.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intrinsics::FixedIntrinsic;

    #[test]
    fn memory_layout_matches_runtime() {
        assert!(runtime_nix().contains(&format!("chunkWords = {MEMORY_CHUNK_WORDS};")));
        assert!(runtime_nix().contains(&format!("groupChunks = {MEMORY_GROUP_CHUNKS};")));
    }

    #[test]
    fn generated_runtime_function_metadata_matches_calls() {
        assert_eq!(RuntimeFunction::I32Add.path(), &["i32", "add"]);
        assert_eq!(RuntimeFunction::I32Add.arity(), 2);
        assert_eq!(
            RuntimeFunction::I32Add.result_kind(),
            RuntimeResultKind::Value
        );

        let call = RuntimeFunction::I32Add.call([Expr::int(1), Expr::int(2)]);
        assert_eq!(call.render(), "rt.i32.add 1 2");
    }

    #[test]
    fn intrinsics_map_to_runtime_functions() {
        assert_eq!(
            FixedIntrinsic::MemGrow.runtime_function(),
            RuntimeFunction::MemGrow
        );
    }
}
