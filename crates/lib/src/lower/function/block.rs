use std::collections::BTreeSet;

use cranelift_codegen::ir::Block;
use cranelift_codegen::ir::Inst;
use cranelift_codegen::ir::{self};
use snafu::OptionExt;

use crate::lower::error::EmptyBlockSnafu;
use crate::lower::error::Result;
use crate::lower::function::cfg::ControlFlow;
use crate::lower::function::inline::inline_single_use_values;
use crate::lower::inst::Force;
use crate::lower::inst::InstCtx;
use crate::lower::inst::value_expr;
use crate::lower::lambda_chain;
use crate::lower::names::ST;
use crate::lower::names::block_name;
use crate::lower::names::value_name;
use crate::nix::AttrBind;
use crate::nix::AttrKey;
use crate::nix::Expr;
use crate::translator::NixModule;

/// Single-edge blocks nest at most this deep before they are emitted as
/// separate block functions, which bounds recursion in the lowering and in
/// the printer.
pub(super) const MAX_INLINE_DEPTH: usize = 32;

pub(in crate::lower) struct BlockLowering<'a> {
    pub(in crate::lower) module: &'a NixModule,
    pub(in crate::lower) func: &'a ir::Function,
    pub(in crate::lower) vmctx: ir::Value,
    pub(super) cfg: &'a ControlFlow,
    pub(in crate::lower) offset_adds: &'a BTreeSet<Inst>,
    /// Single-edge blocks that were called instead of inlined because the
    /// nesting limit was reached.
    pub(super) spilled: Vec<Block>,
}

impl BlockLowering<'_> {
    /// Lowers every block that needs its own function: the entry, blocks with
    /// several incoming edges, loop headers and blocks spilled at the nesting
    /// limit. Every other block is inlined into its only predecessor.
    pub(super) fn lower_blocks(&mut self, entry: Block) -> Result<Vec<AttrBind>> {
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
            let lowered = self.lower_block(block)?;
            binds.push(AttrBind::new(AttrKey::ident(block_name(block)), lowered));
            pending.extend(self.spilled.drain(..).rev());
        }
        Ok(binds)
    }

    /// `params…: free values…: st: body`.
    fn lower_block(&mut self, block: Block) -> Result<Expr> {
        let mut params = self.block_params(block);
        params.extend(self.cfg.free_values(block).iter().map(|v| value_name(*v)));
        params.push(ST.into());

        let body = self.lower_block_body(block, 0)?;
        Ok(lambda_chain(&params, body))
    }

    /// Block parameters, excluding vmctx.
    pub(super) fn block_params(&self, block: Block) -> Vec<String> {
        self.func
            .dfg
            .block_params(block)
            .iter()
            .filter(|v| **v != self.vmctx)
            .map(|v| value_name(*v))
            .collect()
    }

    pub(super) fn lower_block_body(&mut self, block: Block, depth: usize) -> Result<Expr> {
        let mut insts: Vec<Inst> = self.func.layout.block_insts(block).collect();
        let term = insts.pop().context(EmptyBlockSnafu {
            block: block.as_u32(),
        })?;

        let mut steps = Vec::new();
        let mut st_name = ST.to_string();
        for inst in insts {
            let lowered = InstCtx::new(self, inst, &st_name).lower()?;
            steps.extend(lowered.step);
            if let Some(new_st) = lowered.next_state {
                st_name = new_st;
            }
        }

        let expr = self.lower_terminator(block, term, &st_name, depth)?;
        let lowered = inline_single_use_values(steps, expr);
        let conds = lowered.forces.into_iter().map(Force::condition).collect();
        let expr = Expr::asserts(conds, lowered.body);

        Ok(Expr::let_in(lowered.binds, expr))
    }

    pub(super) fn value_expr(&self, v: ir::Value, st_name: &str) -> Expr {
        value_expr(self.func, v, st_name, self.vmctx)
    }
}
