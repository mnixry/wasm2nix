use std::collections::BTreeSet;

use cranelift_codegen::ir::{self, Inst, InstructionData, Opcode};

/// Finds the additions of a memarg offset in heap address computations.
/// Cranelift computes a heap address as `iadd heap_base, index`, then adds a
/// nonzero offset with `iadd base_and_index, iconst offset`. Wasm adds the
/// offset in 33 bits, so that addition must not wrap: the runtime's bounds
/// check then traps on the full sum. Additions in the program's own index
/// arithmetic still wrap.
pub(super) fn offset_adds(func: &ir::Function) -> BTreeSet<Inst> {
    let defined_by = |value: ir::Value, opcode: Opcode| {
        func.dfg
            .value_def(func.dfg.resolve_aliases(value))
            .inst()
            .is_some_and(|inst| func.dfg.insts[inst].opcode() == opcode)
    };
    let iadd_args = |inst: Inst| match &func.dfg.insts[inst] {
        InstructionData::Binary {
            opcode: Opcode::Iadd,
            args,
        } => Some(*args),
        _ => None,
    };
    let is_base_and_index = |value: ir::Value| {
        func.dfg
            .value_def(func.dfg.resolve_aliases(value))
            .inst()
            .and_then(iadd_args)
            .is_some_and(|args| args.iter().any(|&arg| defined_by(arg, Opcode::GlobalValue)))
    };

    func.layout
        .blocks()
        .flat_map(|block| func.layout.block_insts(block))
        .filter(|&inst| {
            iadd_args(inst)
                .is_some_and(|[lhs, rhs]| is_base_and_index(lhs) && defined_by(rhs, Opcode::Iconst))
        })
        .collect()
}
