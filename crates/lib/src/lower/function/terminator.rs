use cranelift_codegen::ir::{self, Block, Inst, InstructionData, Opcode};

use crate::{
    lower::{
        error::{Result, UnsupportedOpcodeSnafu},
        function::{
            block::{BlockLowering, MAX_INLINE_DEPTH},
            block_name,
        },
        inst::truthy_value_expr,
    },
    nix::{AttrKey, Expr, nix},
};

impl BlockLowering<'_> {
    pub(super) fn lower_terminator(
        &self, block: Block, inst: Inst, st_name: &str, depth: usize,
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

            (Opcode::Trap, _) => {
                let trap_code = data
                    .trap_code()
                    .map_or_else(|| "?".into(), |code| code.to_string());
                nix!(throw(#{format!("wasm2nix: trap {trap_code}")}))
            }

            (Opcode::Trapz, InstructionData::CondTrap { arg, code, .. }) => {
                let c = truthy_value_expr(func, *arg, st_name, self.vmctx);
                // trapz: if c is false then trap.
                nix!(if #{c}
                    then #{lower_done_record(Expr::ident(st_name), vec![])}
                    else throw(#{format!("wasm2nix: trapz {code}")}))
            }

            (Opcode::Trapnz, InstructionData::CondTrap { arg, code, .. }) => {
                let c = truthy_value_expr(func, *arg, st_name, self.vmctx);
                let ok = lower_done_record(Expr::ident(st_name), vec![]);
                // trapnz: if c is true then trap.
                nix!(if #{c} then throw(#{format!("wasm2nix: trapnz {code}")}) else #{ok})
            }

            _ => {
                return UnsupportedOpcodeSnafu {
                    opcode: op.to_string(),
                }
                .fail();
            }
        })
    }

    fn lower_branch_table(
        &self, block: Block, inst: Inst, arg: ir::Value, table: ir::JumpTable, st_name: &str,
        depth: usize,
    ) -> Result<Expr> {
        let index_name = format!("br_table_index_{}", inst.as_u32());
        let targets_name = format!("br_table_targets_{}", inst.as_u32());
        let table_data = &self.func.dfg.jump_tables[table];
        let targets = table_data
            .as_slice()
            .iter()
            .map(|branch| self.lower_block_call(block, *branch, st_name, depth))
            .collect::<Result<Vec<_>>>()?;
        let target_count =
            i64::try_from(targets.len()).expect("branch table target count fits in i64");
        let default = self.lower_block_call(block, table_data.default_block(), st_name, depth)?;
        let selected = Expr::app(
            Expr::select(nix!(builtins), AttrKey::ident("elemAt")),
            vec![
                Expr::ident(targets_name.clone()),
                Expr::ident(index_name.clone()),
            ],
        );
        let in_range = nix!(
            #{Expr::ident(index_name.clone())} >= 0
                && #{Expr::ident(index_name.clone())} < #{target_count}
        );

        Ok(Expr::let_in(
            vec![
                (index_name, self.value_expr(arg, st_name)),
                (targets_name, Expr::list(targets)),
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
        &self, from: Block, call: ir::BlockCall, st_name: &str, depth: usize,
    ) -> Result<Expr> {
        let value_lists = &self.func.dfg.value_lists;
        let target = call.block(value_lists);
        let mut args = call
            .args_slice(value_lists)
            .iter()
            .map(|v| self.value_expr(*v, st_name))
            .collect::<Vec<_>>();

        if self.cfg.is_inlinable(target) && !self.cfg.back_edges.contains(&(from, target)) {
            if depth < MAX_INLINE_DEPTH {
                let mut binds = self
                    .block_params(target)
                    .into_iter()
                    .zip(args)
                    .collect::<Vec<_>>();
                if st_name != "st" {
                    binds.push(("st".into(), Expr::ident(st_name)));
                }
                return Ok(Expr::let_in(
                    binds,
                    self.lower_block_body(target, depth + 1)?,
                ));
            }
            self.spilled.borrow_mut().push(target);
        }

        args.extend(
            self.cfg
                .free_values
                .get(&target)
                .into_iter()
                .flat_map(|values| values.iter())
                .map(|v| self.value_expr(*v, st_name)),
        );
        if self.cfg.back_edges.contains(&(from, target)) {
            Ok(lower_jump_record(target, args, Expr::ident(st_name)))
        } else {
            args.push(Expr::ident(st_name));
            Ok(Expr::app(Expr::ident(block_name(target)), args))
        }
    }
}

/// A jump record for the trampoline; the entry record also carries the
/// `genericClosure` key that later steps derive from it.
pub(super) fn lower_jump_record(target: Block, args: Vec<Expr>, st: Expr) -> Expr {
    let block = block_name(target);
    nix!({ tag = "jump"; block = #{block}; args = #{Expr::list(args)}; st = #{st}; })
}

fn lower_done_record(st: Expr, results: Vec<Expr>) -> Expr {
    nix!({ tag = "done"; st = #{st}; results = #{Expr::list(results)}; })
}
