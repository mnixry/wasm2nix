use cranelift_codegen::ir::ArgumentPurpose;
use cranelift_codegen::ir::{self};
use snafu::OptionExt;
use wasm2nix_runtime::RuntimeFunction;

use super::address;
use super::block::BlockLowering;
use super::cfg::ControlFlow;
use super::terminator::lower_jump_record;
use crate::lower::error::MissingEntryBlockSnafu;
use crate::lower::error::MissingVmContextSnafu;
use crate::lower::error::Result;
use crate::lower::lambda_chain;
use crate::lower::names::ST;
use crate::lower::names::block_name;
use crate::lower::names::value_name;
use crate::nix::AttrKey;
use crate::nix::Expr;
use crate::nix::nix;
use crate::translator::NixModule;

/// `params…: st: …`, running the blocks from the entry block. Blocks call
/// their forward successors directly and return either a done record or, on
/// a loop back edge, a jump record for `rt.trampoline`.
pub(in crate::lower) fn lower_function(
    module: &NixModule, func_index: u32, func: &ir::Function,
) -> Result<Expr> {
    let entry = func
        .layout
        .entry_block()
        .context(MissingEntryBlockSnafu { func_index })?;
    let vmctx = func
        .special_param(ArgumentPurpose::VMContext)
        .context(MissingVmContextSnafu { func_index })?;

    let cfg = ControlFlow::analyze(func, entry, vmctx);
    let offset_adds = address::offset_adds(func);
    let mut lowering = BlockLowering {
        module,
        func,
        vmctx,
        cfg: &cfg,
        offset_adds: &offset_adds,
        spilled: Vec::new(),
    };
    let blocks = Expr::rec_attrset(lowering.lower_blocks(entry)?);

    let mut params = lowering.block_params(entry);
    let mut entry_args = params
        .iter()
        .map(|param| Expr::ident(param.as_str()))
        .chain(
            cfg.free_values(entry)
                .iter()
                .map(|v| Expr::ident(value_name(*v))),
        )
        .collect::<Vec<_>>();
    params.push(ST.into());

    let run = if cfg.back_edges.is_empty() {
        entry_args.push(nix!(st));
        Expr::app(
            Expr::select(nix!(blocks), AttrKey::ident(block_name(entry))),
            entry_args,
        )
    } else {
        let entry_record = lower_jump_record(entry, entry_args, nix!(st));
        RuntimeFunction::Trampoline.call([nix!(blocks), entry_record])
    };
    Ok(lambda_chain(
        &params,
        nix!(let
            blocks = #{blocks};
        in #{run}),
    ))
}
