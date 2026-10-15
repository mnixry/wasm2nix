use std::collections::{BTreeMap, BTreeSet};

use cranelift_codegen::ir::{self, Block, Inst, InstructionData};

fn block_free_values(func: &ir::Function, vmctx: ir::Value) -> BTreeMap<Block, Vec<ir::Value>> {
    let mut result = BTreeMap::new();
    let mut defined_by_block = BTreeMap::new();
    let mut successors_by_block = BTreeMap::new();

    for block in func.layout.blocks() {
        let mut defined = func
            .dfg
            .block_params(block)
            .iter()
            .copied()
            .map(|v| func.dfg.resolve_aliases(v))
            .filter(|v| *v != vmctx)
            .collect::<Vec<_>>();
        let mut free = Vec::new();

        for inst in func.layout.block_insts(block) {
            for value in func.dfg.inst_values(inst) {
                push_free_value(func, value, vmctx, &defined, &mut free);
            }

            for block_call in branch_calls(func, inst) {
                successors_by_block
                    .entry(block)
                    .or_insert_with(Vec::new)
                    .push(block_call.block(&func.dfg.value_lists));
                for value in block_call.args_slice(&func.dfg.value_lists) {
                    push_free_value(func, *value, vmctx, &defined, &mut free);
                }
            }
            defined.extend(
                func.dfg
                    .inst_results(inst)
                    .iter()
                    .copied()
                    .map(|v| func.dfg.resolve_aliases(v)),
            );
        }

        defined_by_block.insert(block, defined);
        result.insert(block, free);
    }

    let mut changed = true;
    while changed {
        changed = false;

        for block in func.layout.blocks() {
            let defined = defined_by_block.get(&block).cloned().unwrap_or_default();
            let successors = successors_by_block.get(&block).cloned().unwrap_or_default();
            let mut free = result.get(&block).cloned().unwrap_or_default();

            for successor in successors {
                let successor_free = result.get(&successor).cloned().unwrap_or_default();
                for value in successor_free {
                    let before = free.len();
                    push_free_value(func, value, vmctx, &defined, &mut free);
                    changed |= free.len() != before;
                }
            }

            result.insert(block, free);
        }
    }

    result
}

/// The block calls an instruction branches through, if it is a branch.
fn branch_calls(func: &ir::Function, inst: Inst) -> Vec<ir::BlockCall> {
    match &func.dfg.insts[inst] {
        InstructionData::Jump { destination, .. } => vec![*destination],
        InstructionData::Brif { blocks, .. } => blocks.to_vec(),
        InstructionData::BranchTable { table, .. } => {
            let table_data = &func.dfg.jump_tables[*table];
            std::iter::once(table_data.default_block())
                .chain(table_data.as_slice().iter().copied())
                .collect()
        }
        _ => Vec::new(),
    }
}

pub(super) struct ControlFlow {
    /// Values each block uses without defining, passed as extra parameters.
    pub(super) free_values: BTreeMap<Block, Vec<ir::Value>>,
    /// Edges into a block that is still on the depth-first search path. Wasm
    /// control flow is reducible, so these are exactly the loop back edges.
    pub(super) back_edges: BTreeSet<(Block, Block)>,
    /// Number of branch edges into each block.
    pub(super) in_edges: BTreeMap<Block, usize>,
}

impl ControlFlow {
    pub(super) fn analyze(func: &ir::Function, entry: Block, vmctx: ir::Value) -> Self {
        let successors = |block: Block| {
            func.layout
                .last_inst(block)
                .map(|inst| branch_calls(func, inst))
                .unwrap_or_default()
                .into_iter()
                .map(|call| call.block(&func.dfg.value_lists))
                .collect::<Vec<_>>()
        };

        let mut back_edges = BTreeSet::new();
        let mut on_path = BTreeSet::from([entry]);
        let mut visited = BTreeSet::from([entry]);
        let mut stack = vec![(entry, successors(entry).into_iter())];
        while let Some((block, pending)) = stack.last_mut() {
            let block = *block;
            if let Some(next) = pending.next() {
                if on_path.contains(&next) {
                    back_edges.insert((block, next));
                } else if visited.insert(next) {
                    on_path.insert(next);
                    stack.push((next, successors(next).into_iter()));
                }
            } else {
                on_path.remove(&block);
                stack.pop();
            }
        }

        let mut in_edges = BTreeMap::new();
        for block in func.layout.blocks() {
            for target in successors(block) {
                *in_edges.entry(target).or_insert(0) += 1;
            }
        }

        Self {
            free_values: block_free_values(func, vmctx),
            back_edges,
            in_edges,
        }
    }

    pub(super) fn is_inlinable(&self, block: Block) -> bool {
        self.in_edges.get(&block) == Some(&1)
    }
}

fn push_free_value(
    func: &ir::Function, value: ir::Value, vmctx: ir::Value, defined: &[ir::Value],
    free: &mut Vec<ir::Value>,
) {
    let value = func.dfg.resolve_aliases(value);
    if value != vmctx && !defined.contains(&value) && !free.contains(&value) {
        free.push(value);
    }
}
