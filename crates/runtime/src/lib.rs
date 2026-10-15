pub mod intrinsics;

use std::path::Path;

pub const DEFAULT_RUNTIME_FILE_NAME: &str = "wasm2nix-rt.nix";

/// Number of u32 words per memory chunk; must match `chunkWords` in `rt.nix`.
pub const MEMORY_CHUNK_WORDS: u64 = 128;

#[must_use]
pub const fn runtime_nix() -> &'static str {
    include_str!("../nix/rt.nix")
}

#[must_use]
pub fn runtime_nix_path() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/nix/rt.nix"))
}

#[must_use]
pub const fn runtime_functions_json() -> &'static str {
    include_str!(concat!(env!("OUT_DIR"), "/runtime-functions.json"))
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
    Global,
}

impl RuntimeNamespace {
    #[must_use]
    pub fn expr(self) -> wasm2nix_ast::Expr {
        let segment = match self {
            Self::Mem => "mem",
            Self::Table => "table",
            Self::Global => "global",
        };
        wasm2nix_ast::Expr::select(
            wasm2nix_ast::Expr::ident("rt"),
            wasm2nix_ast::AttrKey::ident(segment),
        )
    }
}

include!(concat!(env!("OUT_DIR"), "/runtime_calls.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_source_is_runtime_crate_owned() {
        assert!(runtime_nix().contains("wasm2nix runtime"));
        assert!(runtime_nix_path().ends_with("nix/rt.nix"));
    }

    #[test]
    fn memory_chunk_size_matches_runtime() {
        assert!(runtime_nix().contains(&format!("chunkWords = {MEMORY_CHUNK_WORDS};")));
    }

    #[test]
    fn generated_manifest_is_json() {
        let value: serde_json::Value =
            serde_json::from_str(runtime_functions_json()).expect("manifest should be valid JSON");
        assert!(
            value["functions"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
    }

    #[test]
    fn generated_runtime_function_metadata_matches_calls() {
        assert_eq!(RuntimeFunction::I32Add.path(), &["i32", "add"]);
        assert_eq!(RuntimeFunction::I32Add.arity(), 2);
        assert_eq!(
            RuntimeFunction::I32Add.result_kind(),
            RuntimeResultKind::Value
        );

        let call = calls::i32_add(wasm2nix_ast::Expr::int(1), wasm2nix_ast::Expr::int(2));
        assert_eq!(call.render(), "rt.i32.add 1 2");
    }

    #[test]
    fn intrinsic_runtime_mapping_is_runtime_owned() {
        assert_eq!(
            intrinsics::Intrinsic::MemGrow.runtime_function(),
            Some(RuntimeFunction::MemGrow)
        );
        assert_eq!(
            intrinsics::Intrinsic::MemGrow.runtime_result_kind(),
            Some(RuntimeResultKind::Record)
        );
        assert_eq!(
            intrinsics::Intrinsic::ref_func(cranelift_wasm::FuncIndex::from_u32(0))
                .runtime_function(),
            None
        );
    }
}
