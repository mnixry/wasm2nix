//! Function references, as stored in tables and compared by
//! `call_indirect`.

use cranelift_codegen::ir;
use cranelift_wasm::FuncIndex;

use crate::linking::FuncType;
use crate::nix::Expr;
use crate::nix::nix;
use crate::translator::NixModule;

/// The table entry referring to function `func_index`, or `null` for an
/// index the module does not have.
pub(super) fn lower_table_entry(module: &NixModule, func_index: FuncIndex) -> Expr {
    let Some(function) = module.info.functions.get(func_index) else {
        return nix!(null);
    };
    let sig = &module.info.signatures[function.entity];
    nix!({
        module = moduleId;
        func = #{func_index.as_u32()};
        type = #{signature_id(sig)};
    })
}

/// The structural identity `call_indirect` checks, such as `(i32,i64)->(i32)`.
pub(super) fn signature_id(sig: &ir::Signature) -> String {
    let FuncType { params, results } = FuncType::from_signature(sig);
    format!("({})->({})", params.join(","), results.join(","))
}
