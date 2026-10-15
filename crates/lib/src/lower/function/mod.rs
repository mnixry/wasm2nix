//! Lowering of one function: control-flow analysis, blocks, terminators and
//! the single-use inliner.

mod block;
mod cfg;
mod inline;
mod terminator;

use std::cell::RefCell;

use cranelift_codegen::ir::{self, ArgumentPurpose, Block};
use snafu::prelude::*;

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

#[allow(clippy::too_many_lines)]
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
    // record or, on a loop back edge, a jump record for the trampoline below.
    let lowering = BlockLowering {
        module,
        func,
        vmctx,
        cfg: &cfg,
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

    let entry_with_key = lower_jump_record(entry, entry_args, nix!(st), Some(0));
    let apply_args = nix!(func: args: builtins."foldl'"(f: arg: f(arg), func, args));
    let apply_block = nix!(block: args: blockSt:
        (applyArgs(builtins.getAttr(block, blocks), args))(blockSt)
    );
    let force_step = nix!(nextKey == #{nix!(nextKey / 1024)} * 1024);
    let step_next = nix!(let
        next = applyBlock(current.block, current.args, current.st);
        nextKey = current.key + 1;
        nextKeyed = merge(next, { key = nextKey; });
        forceStep = #{force_step};
    in if forceStep then builtins.deepSeq(nextKeyed, nextKeyed) else nextKeyed);
    let step_item =
        nix!(current: if current.tag == "done" then [] else #{Expr::list(vec![step_next])});
    let trace_expr = nix!(builtins.genericClosure({
        startSet = [#{entry_with_key}];
        operator = stepItem;
    }));
    let result_index = nix!(builtins.length(trace) - 1);
    let body = nix!(let
        blocks = #{blocks_attrset};
        applyArgs = #{apply_args};
        applyBlock = #{apply_block};
        stepItem = #{step_item};
        trace = #{trace_expr};
        result = builtins.elemAt(trace, #{result_index});
    in {
        st = result.st;
        results = result.results;
    });
    Ok(lambda_chain(&params, body))
}

fn block_name(b: Block) -> String {
    format!("b{}", b.as_u32())
}
