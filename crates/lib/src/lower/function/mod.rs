//! Lowering of one function: control-flow analysis, blocks, terminators and
//! the single-use inliner.

mod address;
mod block;
mod cfg;
mod inline;
mod terminator;

use std::cell::RefCell;

use cranelift_codegen::ir::{self, ArgumentPurpose, Block};
use snafu::prelude::*;
use wasm2nix_runtime::RuntimeFunction;

use crate::{
    lower::{
        error::{MissingEntryBlockSnafu, MissingVmContextSnafu, Result},
        function::{block::BlockLowering, cfg::ControlFlow, terminator::lower_jump_record},
        inst::value_name,
        lambda_chain,
    },
    nix::{AttrKey, Expr, nix},
    translator::NixModule,
};

pub(super) fn lower_function(
    module: &NixModule, func_index: u32, func: &ir::Function,
) -> Result<Expr> {
    let entry = func
        .layout
        .entry_block()
        .context(MissingEntryBlockSnafu { func_index })?;

    let vmctx = func
        .special_param(ArgumentPurpose::VMContext)
        .context(MissingVmContextSnafu { func_index })?;

    // Function parameters: entry block params excluding vmctx, then `st`.
    let mut params: Vec<String> = func
        .dfg
        .block_params(entry)
        .iter()
        .copied()
        .filter(|v| *v != vmctx)
        .map(value_name)
        .collect();
    params.push("st".into());

    let cfg = ControlFlow::analyze(func, entry, vmctx);

    // Blocks call their forward successors directly and return either a done
    // record or, on a loop back edge, a jump record for `rt.trampoline`.
    let offset_adds = address::offset_adds(func);
    let lowering = BlockLowering {
        module,
        func,
        vmctx,
        cfg: &cfg,
        offset_adds: &offset_adds,
        spilled: RefCell::new(Vec::new()),
    };
    let blocks_attrset = Expr::attrset(true, lowering.lower_blocks(entry)?);

    let mut entry_args: Vec<Expr> = func
        .dfg
        .block_params(entry)
        .iter()
        .copied()
        .filter(|v| *v != vmctx)
        .map(|v| Expr::ident(value_name(v)))
        .collect();
    entry_args.extend(
        cfg.free_values
            .get(&entry)
            .into_iter()
            .flat_map(|values| values.iter())
            .map(|v| Expr::ident(value_name(*v))),
    );

    if cfg.back_edges.is_empty() {
        entry_args.push(nix!(st));
        let entry_call = Expr::app(
            Expr::select(nix!(blocks), AttrKey::ident(block_name(entry))),
            entry_args,
        );
        return Ok(lambda_chain(
            &params,
            nix!(let
            blocks = #{blocks_attrset};
        in #{entry_call}),
        ));
    }

    let entry_record = lower_jump_record(entry, entry_args, nix!(st));
    let run = RuntimeFunction::Trampoline.call(vec![nix!(blocks), entry_record]);
    Ok(lambda_chain(
        &params,
        nix!(let
            blocks = #{blocks_attrset};
        in #{run}),
    ))
}

fn block_name(b: Block) -> String {
    format!("b{}", b.as_u32())
}
