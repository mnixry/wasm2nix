use std::collections::BTreeMap;
use std::collections::BTreeSet;

use cranelift_codegen::ir::Block;
use cranelift_codegen::ir::InstructionData;
use cranelift_codegen::ir::{self};

pub(super) struct ControlFlow {
    /// Values each block uses without defining, passed as extra parameters.
    free_values: BTreeMap<Block, Vec<ir::Value>>,
    /// Edges into a block that is still on the depth-first search path. Wasm
    /// control flow is reducible, so these are exactly the loop back edges.
    pub(super) back_edges: BTreeSet<(Block, Block)>,
    /// Number of branch edges into each block.
    in_edges: BTreeMap<Block, usize>,
}

impl ControlFlow {
    pub(super) fn analyze(func: &ir::Function, entry: Block, vmctx: ir::Value) -> Self {
        let successors = func
            .layout
            .blocks()
            .map(|block| (block, successors(func, block)))
            .collect::<BTreeMap<_, _>>();
        let successors_of = |block: Block| successors.get(&block).map_or(&[][..], Vec::as_slice);

        let mut back_edges = BTreeSet::new();
        let mut on_path = BTreeSet::from([entry]);
        let mut visited = BTreeSet::from([entry]);
        let mut stack = vec![(entry, successors_of(entry).iter())];
        while let Some((block, pending)) = stack.last_mut() {
            let block = *block;
            if let Some(&next) = pending.next() {
                if on_path.contains(&next) {
                    back_edges.insert((block, next));
                } else if visited.insert(next) {
                    on_path.insert(next);
                    stack.push((next, successors_of(next).iter()));
                }
            } else {
                on_path.remove(&block);
                stack.pop();
            }
        }

        let mut in_edges = BTreeMap::new();
        for target in successors.values().flatten() {
            *in_edges.entry(*target).or_insert(0) += 1;
        }

        Self {
            free_values: block_free_values(func, vmctx, &successors),
            back_edges,
            in_edges,
        }
    }

    pub(super) fn is_inlinable(&self, block: Block) -> bool {
        self.in_edges.get(&block) == Some(&1)
    }

    pub(super) fn free_values(&self, block: Block) -> &[ir::Value] {
        self.free_values.get(&block).map_or(&[], Vec::as_slice)
    }
}

/// The blocks a block's terminator branches to, in operand order.
fn successors(func: &ir::Function, block: Block) -> Vec<Block> {
    func.layout
        .last_inst(block)
        .map(|inst| branch_calls(func, &func.dfg.insts[inst]))
        .unwrap_or_default()
        .into_iter()
        .map(|call| call.block(&func.dfg.value_lists))
        .collect()
}

/// The block calls a branch instruction passes arguments through.
fn branch_calls(func: &ir::Function, inst: &InstructionData) -> Vec<ir::BlockCall> {
    match inst {
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

/// Values each block uses but neither defines nor receives as a parameter,
/// including those its successors need, to a fixed point.
fn block_free_values(
    func: &ir::Function, vmctx: ir::Value, successors: &BTreeMap<Block, Vec<Block>>,
) -> BTreeMap<Block, Vec<ir::Value>> {
    let mut result = BTreeMap::new();
    let mut defined_by_block = BTreeMap::new();

    for block in func.layout.blocks() {
        let mut defined = func
            .dfg
            .block_params(block)
            .iter()
            .map(|v| func.dfg.resolve_aliases(*v))
            .filter(|v| *v != vmctx)
            .collect::<Vec<_>>();
        let mut free = Vec::new();

        for inst in func.layout.block_insts(block) {
            for value in func.dfg.inst_values(inst) {
                push_free_value(func, value, vmctx, &defined, &mut free);
            }
            for block_call in branch_calls(func, &func.dfg.insts[inst]) {
                for value in block_call.args_slice(&func.dfg.value_lists) {
                    push_free_value(func, *value, vmctx, &defined, &mut free);
                }
            }
            defined.extend(
                func.dfg
                    .inst_results(inst)
                    .iter()
                    .map(|v| func.dfg.resolve_aliases(*v)),
            );
        }

        defined_by_block.insert(block, defined);
        result.insert(block, free);
    }

    let mut changed = true;
    while changed {
        changed = false;
        // Layout order, which the order of the free values depends on.
        for block in func.layout.blocks() {
            let Some(successors) = successors.get(&block) else {
                continue;
            };
            let defined = defined_by_block.get(&block).map_or(&[][..], Vec::as_slice);
            // A self-loop adds nothing, so the block's own entry can be taken
            // out while its successors are read.
            let mut free = result.remove(&block).unwrap_or_default();
            for successor in successors {
                for value in result.get(successor).into_iter().flatten() {
                    let before = free.len();
                    push_free_value(func, *value, vmctx, defined, &mut free);
                    changed |= free.len() != before;
                }
            }
            result.insert(block, free);
        }
    }

    result
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
