use cranelift_codegen::ir::Block;
use cranelift_codegen::ir::Inst;
use cranelift_codegen::ir::InstructionData;
use cranelift_codegen::ir::Opcode;
use cranelift_codegen::ir::{self};
use snafu::OptionExt;

use crate::lower::error::Result;
use crate::lower::error::UnsupportedOpcodeSnafu;
use crate::lower::function::block::BlockLowering;
use crate::lower::function::block::MAX_INLINE_DEPTH;
use crate::lower::inst::truthy_value_expr;
use crate::lower::names::ST;
use crate::lower::names::block_name;
use crate::nix::Expr;
use crate::nix::LetBind;
use crate::nix::nix;

impl BlockLowering<'_> {
    pub(super) fn lower_terminator(
        &mut self, block: Block, inst: Inst, st_name: &str, depth: usize,
    ) -> Result<Expr> {
        let func = self.func;
        let data = &func.dfg.insts[inst];
        let op = data.opcode();

        Ok(match (op, data) {
            (Opcode::Return, InstructionData::MultiAry { args, .. }) => {
                let rvs = args
                    .as_slice(&func.dfg.value_lists)
                    .iter()
                    .map(|v| self.value_expr(*v, st_name))
                    .collect::<Vec<_>>();
                lower_done_record(Expr::ident(st_name), rvs)
            }

            (Opcode::Jump, InstructionData::Jump { destination, .. }) => {
                self.lower_block_call(block, *destination, st_name, depth)?
            }

            (Opcode::Brif, InstructionData::Brif { blocks, arg, .. }) => {
                let cond = truthy_value_expr(func, *arg, st_name, self.vmctx);
                let then_call = self.lower_block_call(block, blocks[0], st_name, depth)?;
                let else_call = self.lower_block_call(block, blocks[1], st_name, depth)?;
                nix!(if #{cond} then #{then_call} else #{else_call})
            }

            (Opcode::BrTable, InstructionData::BranchTable { arg, table, .. }) => {
                self.lower_branch_table(block, inst, *arg, *table, st_name, depth)?
            }

            // `trapz` and `trapnz` do not end blocks; `InstCtx` lowers them.
            (Opcode::Trap, InstructionData::Trap { code, .. }) => {
                nix!(throw(#{format!("wasm2nix: trap {code}")}))
            }

            _ => return UnsupportedOpcodeSnafu { opcode: op }.fail(),
        })
    }

    fn lower_branch_table(
        &mut self, block: Block, inst: Inst, arg: ir::Value, table: ir::JumpTable, st_name: &str,
        depth: usize,
    ) -> Result<Expr> {
        let index_name = format!("br_table_index_{}", inst.as_u32());
        let targets_name = format!("br_table_targets_{}", inst.as_u32());
        #[expect(clippy::indexing_slicing, reason = "the function's own jump table")]
        let table_data = &self.func.dfg.jump_tables[table];
        let targets = table_data
            .as_slice()
            .iter()
            .map(|branch| self.lower_block_call(block, *branch, st_name, depth))
            .collect::<Result<Vec<_>>>()?;
        let target_count = i64::try_from(targets.len())
            .ok()
            .context(UnsupportedOpcodeSnafu {
                opcode: Opcode::BrTable,
            })?;
        let default = self.lower_block_call(block, table_data.default_block(), st_name, depth)?;
        let targets_var = Expr::ident(targets_name.as_str());
        let index_var = Expr::ident(index_name.as_str());
        let selected = nix!(builtins.elemAt(#{targets_var}, #{index_var.clone()}));
        let in_range = nix!(#{index_var.clone()} >= 0 && #{index_var} < #{target_count});

        Ok(Expr::let_in(
            [
                LetBind::new(index_name, self.value_expr(arg, st_name)),
                LetBind::new(targets_name, Expr::list(targets)),
            ],
            nix!(if #{in_range} then #{selected} else #{default}),
        ))
    }

    /// Lowers a branch to `call`. A block with a single incoming edge is
    /// inlined with its parameters bound by `let`; its free values are already
    /// in scope because they are free in, or defined by, the predecessor.
    /// Other forward edges call the target block directly, and back edges
    /// return a jump record to the function's trampoline so that loops do not
    /// grow the Nix call stack.
    fn lower_block_call(
        &mut self, from: Block, call: ir::BlockCall, st_name: &str, depth: usize,
    ) -> Result<Expr> {
        let value_lists = &self.func.dfg.value_lists;
        let target = call.block(value_lists);
        let mut args = call
            .args_slice(value_lists)
            .iter()
            .map(|v| self.value_expr(*v, st_name))
            .collect::<Vec<_>>();

        let back_edge = self.cfg.back_edges.contains(&(from, target));
        if self.cfg.is_inlinable(target) && !back_edge {
            if depth < MAX_INLINE_DEPTH {
                let mut binds = self
                    .block_params(target)
                    .into_iter()
                    .zip(args)
                    .map(|(name, value)| LetBind::new(name, value))
                    .collect::<Vec<_>>();
                if st_name != ST {
                    binds.push(LetBind::new(ST, Expr::ident(st_name)));
                }
                #[expect(clippy::arithmetic_side_effects, reason = "below MAX_INLINE_DEPTH")]
                let depth = depth + 1;
                return Ok(Expr::let_in(binds, self.lower_block_body(target, depth)?));
            }
            self.spilled.push(target);
        }

        args.extend(
            self.cfg
                .free_values(target)
                .iter()
                .map(|v| self.value_expr(*v, st_name)),
        );
        if back_edge {
            Ok(lower_jump_record(target, args, Expr::ident(st_name)))
        } else {
            args.push(Expr::ident(st_name));
            Ok(Expr::app(Expr::ident(block_name(target)), args))
        }
    }
}

/// A record telling the trampoline to call block `target` with `args`.
pub(super) fn lower_jump_record(target: Block, args: Vec<Expr>, st: Expr) -> Expr {
    let block = block_name(target);
    nix!({ tag = "jump"; block = #{block}; args = #{Expr::list(args)}; st = #{st}; })
}

fn lower_done_record(st: Expr, results: Vec<Expr>) -> Expr {
    nix!({ tag = "done"; st = #{st}; results = #{Expr::list(results)}; })
}
