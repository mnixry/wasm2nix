use std::{cell::RefCell, collections::BTreeSet};

use cranelift_codegen::ir::{self, Block, Inst};
use snafu::prelude::*;

use crate::{
    lower::{
        error::{EmptyBlockSnafu, Result},
        function::{block_name, cfg::ControlFlow, inline::inline_single_use_values},
        inst::{Force, Step, lower_inst, value_expr, value_name},
        lambda_chain,
    },
    nix::{AttrKey, Expr},
    translator::NixModule,
};

/// Single-edge blocks nest at most this deep before they are emitted as
/// separate block functions, which bounds recursion in the lowering and in
/// the printer.
pub(super) const MAX_INLINE_DEPTH: usize = 32;

pub(super) struct BlockLowering<'a> {
    pub(super) module: &'a NixModule,
    pub(super) func: &'a ir::Function,
    pub(super) vmctx: ir::Value,
    pub(super) cfg: &'a ControlFlow,
    pub(super) offset_adds: &'a BTreeSet<Inst>,
    /// Single-edge blocks that were called instead of inlined because the
    /// nesting limit was reached.
    pub(super) spilled: RefCell<Vec<Block>>,
}

impl BlockLowering<'_> {
    /// Lowers every block that needs its own function: the entry, blocks with
    /// several incoming edges, loop headers and blocks spilled at the nesting
    /// limit. Every other block is inlined into its only predecessor.
    pub(super) fn lower_blocks(&self, entry: Block) -> Result<Vec<(AttrKey, Expr)>> {
        let mut pending: Vec<Block> = self
            .func
            .layout
            .blocks()
            .filter(|block| *block == entry || !self.cfg.is_inlinable(*block))
            .collect();
        pending.reverse();

        let mut emitted = BTreeSet::new();
        let mut binds = Vec::new();
        while let Some(block) = pending.pop() {
            if !emitted.insert(block) {
                continue;
            }
            binds.push((AttrKey::ident(block_name(block)), self.lower_block(block)?));
            pending.extend(self.spilled.borrow_mut().drain(..).rev());
        }
        Ok(binds)
    }

    fn lower_block(&self, block: Block) -> Result<Expr> {
        let mut params = self.block_params(block);
        params.extend(
            self.cfg
                .free_values
                .get(&block)
                .into_iter()
                .flat_map(|values| values.iter())
                .map(|v| value_name(*v)),
        );
        params.push("st".into());

        let body = self.lower_block_body(block, 0)?;
        Ok(lambda_chain(&params, body))
    }

    /// Block parameters, excluding vmctx.
    pub(super) fn block_params(&self, block: Block) -> Vec<String> {
        self.func
            .dfg
            .block_params(block)
            .iter()
            .copied()
            .filter(|v| *v != self.vmctx)
            .map(value_name)
            .collect()
    }

    pub(super) fn lower_block_body(&self, block: Block, depth: usize) -> Result<Expr> {
        let mut insts: Vec<Inst> = self.func.layout.block_insts(block).collect();
        ensure!(
            !insts.is_empty(),
            EmptyBlockSnafu {
                block: block.as_u32()
            }
        );

        let term = insts.pop().context(EmptyBlockSnafu {
            block: block.as_u32(),
        })?;

        let mut steps: Vec<Step> = Vec::new();
        let mut st_name = "st".to_string();

        for inst in insts {
            let lowered = lower_inst(
                self.module,
                self.func,
                inst,
                &st_name,
                self.vmctx,
                self.offset_adds,
            )?;
            if let Some(step) = lowered.step {
                steps.push(step);
            }
            if let Some(new_st) = lowered.next_state {
                st_name = new_st;
            }
        }

        let expr = self.lower_terminator(block, term, &st_name, depth)?;
        let lowered = inline_single_use_values(steps, expr);
        let conds = lowered.forces.into_iter().map(Force::condition).collect();
        let expr = Expr::asserts(conds, lowered.body);

        Ok(Expr::let_binds(lowered.binds, expr))
    }

    pub(super) fn value_expr(&self, v: ir::Value, st_name: &str) -> Expr {
        value_expr(self.func, v, st_name, self.vmctx)
    }
}
