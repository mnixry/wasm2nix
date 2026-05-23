use crate::intrinsics as intr;
use crate::nix::{AttrKey, BinOp, Expr, UnOp};
use crate::translator::NixModule;
use cranelift_codegen::ir;
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::types::{F32, F64, I8, I16, I32, I64};
use cranelift_codegen::ir::{ArgumentPurpose, Block, Inst, InstructionData, Opcode};
use cranelift_entity::EntityRef;
use cranelift_wasm::{ConstOp, WasmValType};
use std::collections::BTreeMap;

pub fn lower_module(module: &NixModule) -> Expr {
    // Top-level:
    // rt: imports:
    // let moduleId = ...; funcs = rec { ... }; funcRegistry = ...; st0 = ...; st = ...; exports = ...;
    // in exports // { instantiate = _args: { st = st; exports = exports; }; __wasm2nix = { funcs = funcs; }; }
    let funcs_expr = lower_funcs_rec_attrset(module);
    let exports_expr = lower_exports_attrset(module);
    let st0_expr = lower_initial_state(module);
    let instantiate_expr = Expr::lambda(
        "_args",
        Expr::attrset(
            false,
            vec![
                (AttrKey::ident("st"), Expr::ident("st")),
                (AttrKey::ident("exports"), Expr::ident("exports")),
            ],
        ),
    );
    let body = Expr::bin(
        BinOp::Update,
        Expr::ident("exports"),
        Expr::attrset(
            false,
            vec![
                (AttrKey::ident("instantiate"), instantiate_expr),
                (
                    AttrKey::ident("__wasm2nix"),
                    Expr::attrset(
                        false,
                        vec![
                            (AttrKey::ident("funcs"), Expr::ident("funcs")),
                            (AttrKey::ident("moduleId"), Expr::ident("moduleId")),
                        ],
                    ),
                ),
            ],
        ),
    );

    Expr::lambda(
        "rt",
        Expr::lambda(
            "imports",
            Expr::let_in(
                vec![
                    ("moduleId".into(), lower_module_id()),
                    ("funcs".into(), funcs_expr),
                    ("funcRegistry".into(), lower_func_registry()),
                    ("st0".into(), st0_expr),
                    ("st".into(), lower_start_state(module)),
                    ("exports".into(), exports_expr),
                ],
                body,
            ),
        ),
    )
}

fn lower_module_id() -> Expr {
    Expr::if_then_else(
        Expr::app(
            Expr::select(Expr::ident("builtins"), AttrKey::ident("hasAttr")),
            vec![Expr::string("__moduleId"), Expr::ident("imports")],
        ),
        Expr::select(Expr::ident("imports"), AttrKey::string("__moduleId")),
        Expr::string("__self"),
    )
}

fn lower_func_registry() -> Expr {
    Expr::bin(
        BinOp::Update,
        Expr::if_then_else(
            Expr::app(
                Expr::select(Expr::ident("builtins"), AttrKey::ident("hasAttr")),
                vec![Expr::string("__funcRegistry"), Expr::ident("imports")],
            ),
            Expr::select(Expr::ident("imports"), AttrKey::string("__funcRegistry")),
            Expr::attrset(false, vec![]),
        ),
        Expr::app(
            Expr::select(Expr::ident("builtins"), AttrKey::ident("listToAttrs")),
            vec![Expr::list(vec![Expr::attrset(
                false,
                vec![
                    (AttrKey::ident("name"), Expr::ident("moduleId")),
                    (AttrKey::ident("value"), Expr::ident("funcs")),
                ],
            )])],
        ),
    )
}

fn lower_start_state(module: &NixModule) -> Expr {
    if let Some(start) = module.info.start_func {
        let start_func = Expr::select(
            Expr::ident("funcs"),
            AttrKey::ident(format!("f{}", start.as_u32())),
        );
        Expr::select(
            Expr::app(start_func, vec![Expr::ident("st0")]),
            AttrKey::ident("st"),
        )
    } else {
        Expr::ident("st0")
    }
}

fn lower_funcs_rec_attrset(module: &NixModule) -> Expr {
    let mut binds: Vec<(AttrKey, Expr)> = Vec::new();

    for (idx, (module_name, field_name)) in module.info.imported_funcs.iter().enumerate() {
        let imported = Expr::select(
            Expr::select(Expr::ident("imports"), AttrKey::string(module_name.clone())),
            AttrKey::string(field_name.clone()),
        );
        binds.push((AttrKey::ident(format!("f{idx}")), imported));
    }

    for (defined_idx, func) in module.info.function_bodies.iter() {
        let func_index = module.num_func_imports() as u32 + defined_idx.as_u32();
        let name = format!("f{func_index}");
        let lowered = lower_function(module, func_index, func);
        binds.push((AttrKey::ident(name), lowered));
    }

    Expr::attrset(true, binds)
}

fn lower_exports_attrset(module: &NixModule) -> Expr {
    let mut binds: Vec<(AttrKey, Expr)> = Vec::new();

    for (func_index, func_decl) in module.info.functions.iter() {
        if func_decl.export_names.is_empty() {
            continue;
        }
        let f_name = format!("f{}", func_index.as_u32());
        let f_expr = Expr::select(Expr::ident("funcs"), AttrKey::ident(f_name));
        for export_name in &func_decl.export_names {
            // Use string keys for safety (names can contain characters invalid in identifiers).
            binds.push((AttrKey::string(export_name.clone()), f_expr.clone()));
        }
    }

    for (memory_index, memory_decl) in module.info.memories.iter() {
        for export_name in &memory_decl.export_names {
            binds.push((
                AttrKey::string(export_name.clone()),
                lower_memory_export(memory_index.as_u32()),
            ));
        }
    }

    for (table_index, table_decl) in module.info.tables.iter() {
        for export_name in &table_decl.export_names {
            binds.push((
                AttrKey::string(export_name.clone()),
                lower_table_export(table_index.as_u32()),
            ));
        }
    }

    for (global_index, global_decl) in module.info.globals.iter() {
        for export_name in &global_decl.export_names {
            binds.push((
                AttrKey::string(export_name.clone()),
                lower_global_export(global_index.as_u32(), global_decl.entity.wasm_ty),
            ));
        }
    }

    Expr::attrset(false, binds)
}

fn lower_initial_state(module: &NixModule) -> Expr {
    let page_count = module
        .info
        .memories
        .iter()
        .next()
        .map(|(_, memory)| memory.entity.minimum as i64)
        .unwrap_or(0);

    Expr::attrset(
        false,
        vec![
            (
                AttrKey::ident("mem"),
                Expr::attrset(
                    false,
                    vec![
                        (AttrKey::ident("pageCount"), Expr::int(page_count)),
                        (AttrKey::ident("pages"), lower_initial_memory_pages(module)),
                    ],
                ),
            ),
            (AttrKey::ident("globals"), lower_initial_globals(module)),
            (AttrKey::ident("tables"), lower_initial_tables(module)),
            (AttrKey::ident("data"), lower_data_segments(module)),
            (
                AttrKey::ident("elemSegments"),
                lower_element_segments(module),
            ),
        ],
    )
}

fn lower_initial_memory_pages(module: &NixModule) -> Expr {
    let mut words: BTreeMap<u64, u32> = BTreeMap::new();

    for init in &module.info.data_initializers {
        if init.memory_index.index() != 0 || init.base.is_some() {
            continue;
        }

        for (i, byte) in init.data.iter().copied().enumerate() {
            let addr = init.offset + i as u64;
            let word_index = addr / 4;
            let shift = ((addr % 4) as u32) * 8;
            let old_word = words.get(&word_index).copied().unwrap_or(0);
            let mask = !(0xff_u32 << shift);
            let new_word = (old_word & mask) | (u32::from(byte) << shift);
            words.insert(word_index, new_word);
        }
    }

    Expr::attrset(
        false,
        words
            .into_iter()
            .map(|(idx, word)| (AttrKey::string(idx.to_string()), Expr::int(i64::from(word))))
            .collect(),
    )
}

fn lower_data_segments(module: &NixModule) -> Expr {
    Expr::attrset(
        false,
        module
            .info
            .data_segments
            .iter()
            .map(|(idx, data)| {
                (
                    AttrKey::string(idx.as_u32().to_string()),
                    Expr::list(
                        data.iter()
                            .copied()
                            .map(|byte| Expr::int(i64::from(byte)))
                            .collect(),
                    ),
                )
            })
            .collect(),
    )
}

fn lower_initial_globals(module: &NixModule) -> Expr {
    Expr::attrset(
        false,
        module
            .info
            .globals
            .iter()
            .filter_map(|(idx, global)| {
                let init = module
                    .info
                    .global_inits
                    .get(idx)
                    .and_then(|expr| expr.as_ref())?;
                Some((
                    AttrKey::string(idx.as_u32().to_string()),
                    lower_const_expr(init, global.entity.wasm_ty),
                ))
            })
            .collect(),
    )
}

fn lower_initial_tables(module: &NixModule) -> Expr {
    let mut table_binds: Vec<(AttrKey, Expr)> = Vec::new();

    for (table_index, table_decl) in module.info.tables.iter() {
        let mut elems: BTreeMap<u32, Expr> = BTreeMap::new();
        for init in &module.info.table_initializers {
            if init.table_index != table_index || init.base.is_some() {
                continue;
            }

            for (i, func_index) in init.elements.iter().copied().enumerate() {
                let slot = init.offset + i as u32;
                elems.insert(slot, lower_table_entry(module, func_index));
            }
        }

        let elems_expr = Expr::attrset(
            false,
            elems
                .into_iter()
                .map(|(idx, entry)| (AttrKey::string(idx.to_string()), entry))
                .collect(),
        );
        table_binds.push((
            AttrKey::string(table_index.as_u32().to_string()),
            Expr::attrset(
                false,
                vec![
                    (
                        AttrKey::ident("size"),
                        Expr::int(i64::from(table_decl.entity.minimum)),
                    ),
                    (AttrKey::ident("elems"), elems_expr),
                ],
            ),
        ));
    }

    Expr::attrset(false, table_binds)
}

fn lower_element_segments(module: &NixModule) -> Expr {
    Expr::attrset(
        false,
        module
            .info
            .element_segments
            .iter()
            .map(|(idx, elements)| {
                (
                    AttrKey::string(idx.as_u32().to_string()),
                    Expr::list(
                        elements
                            .iter()
                            .copied()
                            .map(|func_index| lower_table_entry(module, func_index))
                            .collect(),
                    ),
                )
            })
            .collect(),
    )
}

fn lower_table_entry(module: &NixModule, func_index: cranelift_wasm::FuncIndex) -> Expr {
    let Some(function) = module.info.functions.get(func_index) else {
        return Expr::ident("null");
    };
    let sig_index = module.info.canonical_type_index(function.entity).as_u32();
    Expr::attrset(
        false,
        vec![
            (AttrKey::ident("module"), Expr::ident("moduleId")),
            (
                AttrKey::ident("func"),
                Expr::int(i64::from(func_index.as_u32())),
            ),
            (AttrKey::ident("type"), Expr::int(i64::from(sig_index))),
        ],
    )
}

fn lower_const_expr(expr: &cranelift_wasm::ConstExpr, ty: WasmValType) -> Expr {
    let ops = expr.ops();
    if ops.len() != 1 {
        return Expr::app(
            builtin_throw(),
            vec![Expr::string(
                "wasm2nix: unsupported compound const expression",
            )],
        );
    }

    match (ops[0], ty) {
        (ConstOp::I32Const(value), WasmValType::I32) => Expr::int(i64::from(value as u32)),
        (ConstOp::I64Const(value), WasmValType::I64) => Expr::int(value),
        (ConstOp::F32Const(bits), WasmValType::F32) => Expr::int(i64::from(bits)),
        (ConstOp::F64Const(bits), WasmValType::F64) if bits <= i64::MAX as u64 => {
            Expr::int(bits as i64)
        }
        _ => Expr::app(
            builtin_throw(),
            vec![Expr::string("wasm2nix: unsupported const expression")],
        ),
    }
}

fn lower_memory_export(_memory_index: u32) -> Expr {
    Expr::select(Expr::ident("rt"), AttrKey::ident("mem"))
}

fn lower_table_export(_table_index: u32) -> Expr {
    Expr::select(Expr::ident("rt"), AttrKey::ident("table"))
}

fn lower_global_export(global_index: u32, ty: WasmValType) -> Expr {
    let get = Expr::lambda(
        "st",
        Expr::app(
            rt_fn(&["global", global_get_name(ty)]),
            vec![Expr::int(i64::from(global_index)), Expr::ident("st")],
        ),
    );
    let set = Expr::lambda(
        "value",
        Expr::lambda(
            "st",
            Expr::app(
                rt_fn(&["global", global_set_name(ty)]),
                vec![
                    Expr::int(i64::from(global_index)),
                    Expr::ident("value"),
                    Expr::ident("st"),
                ],
            ),
        ),
    );

    Expr::attrset(
        false,
        vec![(AttrKey::ident("get"), get), (AttrKey::ident("set"), set)],
    )
}

fn lower_function(_module: &NixModule, _func_index: u32, func: &ir::Function) -> Expr {
    let entry = func
        .layout
        .entry_block()
        .expect("function must have an entry block");

    let vmctx = func
        .special_param(ArgumentPurpose::VMContext)
        .expect("translator should add VMContext param");

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

    let free_values = block_free_values(func, vmctx);

    // Blocks: `blocks = rec { b0 = ...; b1 = ...; }; in blocks.b0 <args> st`
    let mut block_binds: Vec<(AttrKey, Expr)> = Vec::new();
    for block in func.layout.blocks() {
        let bname = block_name(block);
        let bexpr = lower_block(func, block, vmctx, &free_values);
        block_binds.push((AttrKey::ident(bname), bexpr));
    }
    let blocks_attrset = Expr::attrset(true, block_binds);

    let entry_call = {
        let b0 = Expr::select(Expr::ident("blocks"), AttrKey::ident(block_name(entry)));
        let mut args: Vec<Expr> = func
            .dfg
            .block_params(entry)
            .iter()
            .copied()
            .filter(|v| *v != vmctx)
            .map(|v| Expr::ident(value_name(v)))
            .collect();
        args.extend(
            free_values
                .get(&entry)
                .into_iter()
                .flat_map(|values| values.iter())
                .map(|v| Expr::ident(value_name(*v))),
        );
        args.push(Expr::ident("st"));
        Expr::app(b0, args)
    };

    let body = Expr::let_in(vec![("blocks".into(), blocks_attrset)], entry_call);
    lambda_chain(&params, body)
}

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

            match &func.dfg.insts[inst] {
                InstructionData::Jump { destination, .. } => {
                    successors_by_block
                        .entry(block)
                        .or_insert_with(Vec::new)
                        .push(destination.block(&func.dfg.value_lists));
                    for value in destination.args_slice(&func.dfg.value_lists) {
                        push_free_value(func, *value, vmctx, &defined, &mut free);
                    }
                }
                InstructionData::Brif { blocks, .. } => {
                    for block_call in blocks {
                        successors_by_block
                            .entry(block)
                            .or_insert_with(Vec::new)
                            .push(block_call.block(&func.dfg.value_lists));
                        for value in block_call.args_slice(&func.dfg.value_lists) {
                            push_free_value(func, *value, vmctx, &defined, &mut free);
                        }
                    }
                }
                InstructionData::BranchTable { table, .. } => {
                    let table_data = &func.dfg.jump_tables[*table];
                    let successors = successors_by_block.entry(block).or_insert_with(Vec::new);
                    successors.push(table_data.default_block().block(&func.dfg.value_lists));
                    for block_call in table_data.as_slice() {
                        successors.push(block_call.block(&func.dfg.value_lists));
                    }
                }
                _ => {}
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
                for value in result.get(&successor).cloned().unwrap_or_default() {
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

fn push_free_value(
    func: &ir::Function,
    value: ir::Value,
    vmctx: ir::Value,
    defined: &[ir::Value],
    free: &mut Vec<ir::Value>,
) {
    let value = func.dfg.resolve_aliases(value);
    if value != vmctx && !defined.contains(&value) && !free.contains(&value) {
        free.push(value);
    }
}

fn lower_block(
    func: &ir::Function,
    block: Block,
    vmctx: ir::Value,
    free_values: &BTreeMap<Block, Vec<ir::Value>>,
) -> Expr {
    // Block parameters: block params excluding vmctx, then `st`.
    let mut params: Vec<String> = func
        .dfg
        .block_params(block)
        .iter()
        .copied()
        .filter(|v| *v != vmctx)
        .map(value_name)
        .collect();
    params.extend(
        free_values
            .get(&block)
            .into_iter()
            .flat_map(|values| values.iter())
            .map(|v| value_name(*v)),
    );
    params.push("st".into());

    let body = lower_block_body(func, block, vmctx, free_values);
    lambda_chain(&params, body)
}

struct Step {
    binds: Vec<(String, Expr)>,
    force: Option<Expr>,
}

fn lower_block_body(
    func: &ir::Function,
    block: Block,
    vmctx: ir::Value,
    free_values: &BTreeMap<Block, Vec<ir::Value>>,
) -> Expr {
    let mut insts: Vec<Inst> = func.layout.block_insts(block).collect();
    if insts.is_empty() {
        return Expr::app(builtin_throw(), vec![Expr::string("wasm2nix: empty block")]);
    }

    let term = insts.pop().unwrap();

    let mut steps: Vec<Step> = Vec::new();
    let mut st_name = "st".to_string();

    for inst in insts {
        let (step, new_st) = lower_inst(func, inst, &st_name, vmctx);
        if let Some(step) = step {
            steps.push(step);
        }
        if let Some(new_st) = new_st {
            st_name = new_st;
        }
    }

    let mut expr = lower_terminator(func, term, &st_name, vmctx, free_values);
    for step in steps.into_iter().rev() {
        let body = if let Some(force) = step.force {
            Expr::app(builtin_seq(), vec![force, expr])
        } else {
            expr
        };
        expr = Expr::let_in(step.binds, body);
    }

    expr
}

fn lower_inst(
    func: &ir::Function,
    inst: Inst,
    st_name: &str,
    vmctx: ir::Value,
) -> (Option<Step>, Option<String>) {
    let data = &func.dfg.insts[inst];
    let op = data.opcode();
    let results = func.dfg.inst_results(inst);

    // Helper to always seq on the first result (if any).
    let seq_on_result = |binds: Vec<(String, Expr)>| -> Option<Step> {
        let force = results
            .first()
            .map(|v| Expr::ident(value_name(*v)))
            .or_else(|| binds.first().map(|(n, _)| Expr::ident(n.clone())));
        Some(Step { binds, force })
    };

    if op == Opcode::Bitcast {
        let out = results[0];
        let arg = data.arguments(&func.dfg.value_lists)[0];
        let a = value_expr(func, arg, st_name, vmctx);
        let binds = vec![(value_name(out), a)];
        return (seq_on_result(binds), None);
    }

    match data {
        InstructionData::UnaryImm { imm, .. } if op == Opcode::Iconst => {
            let dst = results[0];
            let ty = func.dfg.value_type(dst);
            let v = i64::from(*imm);
            let nix_int = if ty == I32 {
                // represent i32 as u32 in i64 range
                i64::from(v as u32)
            } else {
                v
            };
            let binds = vec![(value_name(dst), Expr::int(nix_int))];
            (seq_on_result(binds), None)
        }

        InstructionData::UnaryIeee32 { imm, .. } if op == Opcode::F32const => {
            let dst = results[0];
            let bits = imm.bits();
            let binds = vec![(value_name(dst), Expr::int(i64::from(bits)))];
            (seq_on_result(binds), None)
        }

        InstructionData::UnaryIeee64 { imm, .. } if op == Opcode::F64const => {
            let dst = results[0];
            let bits = imm.bits();
            let binds = vec![(value_name(dst), Expr::int(bits as i64))];
            (seq_on_result(binds), None)
        }

        InstructionData::UnaryGlobalValue { .. }
            if matches!(
                op,
                Opcode::GlobalValue | Opcode::SymbolValue | Opcode::TlsValue
            ) =>
        {
            // v0: treat all global values as 0. (Used for symbolic heap base.)
            let dst = results[0];
            let binds = vec![(value_name(dst), Expr::int(0))];
            (seq_on_result(binds), None)
        }

        InstructionData::Unary { arg, .. } => {
            match op {
                Opcode::Uextend => {
                    let out = results[0];
                    let in_ty = func.dfg.value_type(*arg);
                    let out_ty = func.dfg.value_type(out);
                    let a = value_expr(func, *arg, st_name, vmctx);

                    let e = if in_ty == I8 && out_ty == I32 {
                        Expr::if_then_else(a, Expr::int(1), Expr::int(0))
                    } else {
                        // For now, rely on runtime for non-bool extensions.
                        Expr::app(
                            rt_fn(&["conv", "uextend"]),
                            vec![a, Expr::string(out_ty.to_string())],
                        )
                    };
                    let binds = vec![(value_name(out), e)];
                    (seq_on_result(binds), None)
                }
                Opcode::Sextend => {
                    let out = results[0];
                    let in_ty = func.dfg.value_type(*arg);
                    let out_ty = func.dfg.value_type(out);
                    let a = value_expr(func, *arg, st_name, vmctx);
                    let e = match (in_ty, out_ty) {
                        (I8, I32) => Expr::app(rt_fn(&["conv", "sign_extend_8"]), vec![a]),
                        (I16, I32) => Expr::app(rt_fn(&["conv", "sign_extend_16"]), vec![a]),
                        (I8, I64) => Expr::app(rt_fn(&["conv", "sign_extend_8_i64"]), vec![a]),
                        (I16, I64) => Expr::app(rt_fn(&["conv", "sign_extend_16_i64"]), vec![a]),
                        (I32, I64) => Expr::app(rt_fn(&["conv", "sign_extend_32"]), vec![a]),
                        _ => Expr::app(rt_fn(&["conv", "sextend"]), vec![a]),
                    };
                    let binds = vec![(value_name(out), e)];
                    (seq_on_result(binds), None)
                }
                Opcode::Ireduce => {
                    let out = results[0];
                    let a = value_expr(func, *arg, st_name, vmctx);
                    let binds = vec![(
                        value_name(out),
                        Expr::app(rt_fn(&["conv", "ireduce"]), vec![a]),
                    )];
                    (seq_on_result(binds), None)
                }
                Opcode::Bitcast => {
                    let out = results[0];
                    // With our representation, bitcast between i32<->f32 and i64<->f64 is identity.
                    let a = value_expr(func, *arg, st_name, vmctx);
                    let binds = vec![(value_name(out), a)];
                    (seq_on_result(binds), None)
                }
                Opcode::FcvtToSint
                | Opcode::FcvtToUint
                | Opcode::FcvtToSintSat
                | Opcode::FcvtToUintSat => {
                    let out = results[0];
                    let in_ty = func.dfg.value_type(*arg);
                    let out_ty = func.dfg.value_type(out);
                    let float_key = if in_ty == F32 {
                        "f32"
                    } else if in_ty == F64 {
                        "f64"
                    } else {
                        "unknown"
                    };
                    let int_key = if out_ty == I32 {
                        "i32"
                    } else if out_ty == I64 {
                        "i64"
                    } else {
                        "unknown"
                    };
                    let conv_name = match op {
                        Opcode::FcvtToSint => "trunc_s",
                        Opcode::FcvtToUint => "trunc_u",
                        Opcode::FcvtToSintSat => "trunc_sat_s",
                        Opcode::FcvtToUintSat => "trunc_sat_u",
                        _ => unreachable!(),
                    };
                    let a = value_expr(func, *arg, st_name, vmctx);
                    let binds = vec![(
                        value_name(out),
                        Expr::app(rt_fn(&["conv", conv_name, float_key, int_key]), vec![a]),
                    )];
                    (seq_on_result(binds), None)
                }
                Opcode::FcvtFromSint | Opcode::FcvtFromUint => {
                    let out = results[0];
                    let in_ty = func.dfg.value_type(*arg);
                    let out_ty = func.dfg.value_type(out);
                    let int_key = if in_ty == I32 {
                        "i32"
                    } else if in_ty == I64 {
                        "i64"
                    } else {
                        "unknown"
                    };
                    let float_key = if out_ty == F32 {
                        "f32"
                    } else if out_ty == F64 {
                        "f64"
                    } else {
                        "unknown"
                    };
                    let conv_name = match op {
                        Opcode::FcvtFromSint => "convert_s",
                        Opcode::FcvtFromUint => "convert_u",
                        _ => unreachable!(),
                    };
                    let a = value_expr(func, *arg, st_name, vmctx);
                    let binds = vec![(
                        value_name(out),
                        Expr::app(rt_fn(&["conv", conv_name, int_key, float_key]), vec![a]),
                    )];
                    (seq_on_result(binds), None)
                }
                Opcode::Fpromote | Opcode::Fdemote => {
                    let out = results[0];
                    let a = value_expr(func, *arg, st_name, vmctx);
                    let (ty_key, op_name) = if op == Opcode::Fpromote {
                        ("f64", "promote_f32")
                    } else {
                        ("f32", "demote_f64")
                    };
                    let binds = vec![(
                        value_name(out),
                        Expr::app(rt_fn(&[ty_key, op_name]), vec![a]),
                    )];
                    (seq_on_result(binds), None)
                }
                Opcode::Bnot => {
                    let out = results[0];
                    let a = value_expr(func, *arg, st_name, vmctx);
                    let binds =
                        vec![(value_name(out), Expr::app(rt_fn(&["int", "bnot"]), vec![a]))];
                    (seq_on_result(binds), None)
                }
                Opcode::Ineg => {
                    let out = results[0];
                    let a = value_expr(func, *arg, st_name, vmctx);
                    let binds = vec![(value_name(out), Expr::un(UnOp::Neg, a))];
                    (seq_on_result(binds), None)
                }
                Opcode::Clz | Opcode::Ctz | Opcode::Popcnt => {
                    let out = results[0];
                    let a = value_expr(func, *arg, st_name, vmctx);
                    let out_ty = func.dfg.value_type(out);
                    let ty_key = if out_ty == I32 {
                        "i32"
                    } else if out_ty == I64 {
                        "i64"
                    } else {
                        "unknown"
                    };
                    let op_name = match op {
                        Opcode::Clz => "clz",
                        Opcode::Ctz => "ctz",
                        Opcode::Popcnt => "popcnt",
                        _ => unreachable!(),
                    };
                    let binds = vec![(
                        value_name(out),
                        Expr::app(rt_fn(&[ty_key, op_name]), vec![a]),
                    )];
                    (seq_on_result(binds), None)
                }
                Opcode::Fneg | Opcode::Fabs | Opcode::Sqrt => {
                    let out = results[0];
                    let a = value_expr(func, *arg, st_name, vmctx);
                    let out_ty = func.dfg.value_type(out);
                    let ty_key = if out_ty == F32 { "f32" } else { "f64" };
                    let op_name = match op {
                        Opcode::Fneg => "neg",
                        Opcode::Fabs => "abs",
                        Opcode::Sqrt => "sqrt",
                        _ => unreachable!(),
                    };
                    let binds = vec![(
                        value_name(out),
                        Expr::app(rt_fn(&[ty_key, op_name]), vec![a]),
                    )];
                    (seq_on_result(binds), None)
                }
                _ => {
                    let binds = unsupported_binds(
                        inst,
                        results,
                        format!("wasm2nix: unimplemented unary op {op}"),
                    );
                    (Some(Step { binds, force: None }), None)
                }
            }
        }

        InstructionData::Binary {
            args: [arg1, arg2], ..
        } => {
            let a = value_expr(func, *arg1, st_name, vmctx);
            let b = value_expr(func, *arg2, st_name, vmctx);
            let out = results[0];
            let out_ty = func.dfg.value_type(out);
            let ty_key = if out_ty == I32 {
                "i32"
            } else if out_ty == I64 {
                "i64"
            } else if out_ty == F32 {
                "f32"
            } else if out_ty == F64 {
                "f64"
            } else if out_ty == I8 {
                "b1"
            } else {
                "unknown"
            };

            let op_name = match op {
                Opcode::Iadd => "add",
                Opcode::Isub => "sub",
                Opcode::Imul => "mul",
                Opcode::Udiv => "udiv",
                Opcode::Sdiv => "sdiv",
                Opcode::Urem => "urem",
                Opcode::Srem => "srem",
                Opcode::Band => "band",
                Opcode::Bor => "bor",
                Opcode::Bxor => "bxor",
                Opcode::Ishl => "shl",
                Opcode::Ushr => "shr_u",
                Opcode::Sshr => "shr_s",
                Opcode::Rotl => "rotl",
                Opcode::Rotr => "rotr",
                Opcode::Fadd => "add",
                Opcode::Fsub => "sub",
                Opcode::Fmul => "mul",
                Opcode::Fdiv => "div",
                Opcode::Fmin => "min",
                Opcode::Fmax => "max",
                Opcode::Fcopysign => "copysign",
                _ => unimplemented!("wasm2nix: unimplemented binary op {op:?}"),
            };

            let e = Expr::app(rt_fn(&[ty_key, op_name]), vec![a, b]);
            let binds = vec![(value_name(out), e)];
            (seq_on_result(binds), None)
        }

        InstructionData::BinaryImm64 { arg, imm, .. } => {
            let a = value_expr(func, *arg, st_name, vmctx);
            let out = results[0];
            let out_ty = func.dfg.value_type(out);
            let imm64 = i64::from(*imm);
            let imm_expr = if out_ty == I32 {
                Expr::int(i64::from(imm64 as u32))
            } else {
                Expr::int(imm64)
            };

            let ty_key = if out_ty == I32 { "i32" } else { "i64" };
            let op_name = match op {
                Opcode::IaddImm => "add",
                Opcode::ImulImm => "mul",
                Opcode::BandImm => "band",
                Opcode::BorImm => "bor",
                Opcode::BxorImm => "bxor",
                Opcode::IshlImm => "shl",
                Opcode::UshrImm => "shr_u",
                Opcode::SshrImm => "shr_s",
                _ => {
                    let binds = unsupported_binds(
                        inst,
                        results,
                        format!("wasm2nix: unimplemented binary-imm op {op}"),
                    );
                    return (Some(Step { binds, force: None }), None);
                }
            };

            let e = Expr::app(rt_fn(&[ty_key, op_name]), vec![a, imm_expr]);
            let binds = vec![(value_name(out), e)];
            (seq_on_result(binds), None)
        }

        InstructionData::Ternary { args, .. }
            if matches!(op, Opcode::Select | Opcode::SelectSpectreGuard) =>
        {
            let out = results[0];
            let cond = truthy_value_expr(func, args[0], st_name, vmctx);
            let then_value = value_expr(func, args[1], st_name, vmctx);
            let else_value = value_expr(func, args[2], st_name, vmctx);
            let binds = vec![(
                value_name(out),
                Expr::if_then_else(cond, then_value, else_value),
            )];
            (seq_on_result(binds), None)
        }

        InstructionData::IntCompare { args, cond, .. } if op == Opcode::Icmp => {
            let out = results[0];
            let a = value_expr(func, args[0], st_name, vmctx);
            let b = value_expr(func, args[1], st_name, vmctx);
            let cmp_ns = if func.dfg.value_type(args[0]) == I64 {
                "cmp_i64"
            } else {
                "cmp"
            };
            let e = Expr::app(rt_fn(&[cmp_ns, intcc_name(*cond)]), vec![a, b]);
            let binds = vec![(value_name(out), e)];
            (seq_on_result(binds), None)
        }

        InstructionData::IntCompareImm { arg, cond, imm, .. } if op == Opcode::IcmpImm => {
            let out = results[0];
            let a = value_expr(func, *arg, st_name, vmctx);
            let ty = func.dfg.value_type(*arg);
            let imm64 = i64::from(*imm);
            let imm_expr = if ty == I32 {
                Expr::int(i64::from(imm64 as u32))
            } else {
                Expr::int(imm64)
            };
            let cmp_ns = if ty == I64 { "cmp_i64" } else { "cmp" };
            let e = Expr::app(rt_fn(&[cmp_ns, intcc_name(*cond)]), vec![a, imm_expr]);
            let binds = vec![(value_name(out), e)];
            (seq_on_result(binds), None)
        }

        InstructionData::FloatCompare { args, cond, .. } if op == Opcode::Fcmp => {
            let out = results[0];
            let a = value_expr(func, args[0], st_name, vmctx);
            let b = value_expr(func, args[1], st_name, vmctx);
            let ty = func.dfg.value_type(args[0]);
            let float_ns = if ty == F32 { "f32" } else { "f64" };
            let e = Expr::app(rt_fn(&["fcmp", float_ns, floatcc_name(*cond)]), vec![a, b]);
            let binds = vec![(value_name(out), e)];
            (seq_on_result(binds), None)
        }

        InstructionData::Load { arg, offset, .. } => {
            let out = results[0];
            let out_ty = func.dfg.value_type(out);
            let base = value_expr(func, *arg, st_name, vmctx);
            let off: i32 = (*offset).into();
            let addr = if off == 0 {
                base
            } else {
                Expr::bin(BinOp::Add, base, Expr::int(off as i64))
            };
            let st = Expr::ident(st_name);
            let e = Expr::app(rt_fn(&["mem", load_fn_name(op, out_ty)]), vec![addr, st]);
            let binds = vec![(value_name(out), e)];
            (seq_on_result(binds), None)
        }

        InstructionData::Store { args, offset, .. } => {
            // Store args are [value, addr]
            let val = value_expr(func, args[0], st_name, vmctx);
            let base = value_expr(func, args[1], st_name, vmctx);
            let off: i32 = (*offset).into();
            let addr = if off == 0 {
                base
            } else {
                Expr::bin(BinOp::Add, base, Expr::int(off as i64))
            };

            let new_st = format!("st_{}", inst.as_u32());
            let e = Expr::app(
                rt_fn(&["mem", store_fn_name(op, func.dfg.value_type(args[0]))]),
                vec![val, addr, Expr::ident(st_name)],
            );
            let binds = vec![(new_st.clone(), e)];
            (
                Some(Step {
                    binds,
                    force: Some(Expr::ident(new_st.clone())),
                }),
                Some(new_st),
            )
        }

        InstructionData::CondTrap { arg, code, .. }
            if matches!(op, Opcode::Trapz | Opcode::Trapnz) =>
        {
            let trap_name = format!("_trap_{}", inst.as_u32());
            let c = truthy_value_expr(func, *arg, st_name, vmctx);
            let trap_expr = Expr::app(
                builtin_throw(),
                vec![Expr::string(format!("wasm2nix: trap {code}"))],
            );
            let trap = if op == Opcode::Trapz {
                Expr::if_then_else(c, Expr::int(0), trap_expr)
            } else {
                Expr::if_then_else(c, trap_expr, Expr::int(0))
            };
            (
                Some(Step {
                    binds: vec![(trap_name.clone(), trap)],
                    force: Some(Expr::ident(trap_name)),
                }),
                None,
            )
        }

        InstructionData::Trap { code, .. } => {
            let trap_name = format!("_trap_{}", inst.as_u32());
            (
                Some(Step {
                    binds: vec![(
                        trap_name.clone(),
                        Expr::app(
                            builtin_throw(),
                            vec![Expr::string(format!("wasm2nix: trap {code}"))],
                        ),
                    )],
                    force: Some(Expr::ident(trap_name)),
                }),
                None,
            )
        }

        // Calls: use analyze_call() to get func_ref and args.
        _ if op == Opcode::Call => {
            let call_info = data.analyze_call(&func.dfg.value_lists);
            let (fref, args) = match call_info {
                ir::instructions::CallInfo::Direct(fref, args) => (fref, args),
                _ => {
                    let binds = unsupported_binds(
                        inst,
                        results,
                        "wasm2nix: unsupported call kind".to_string(),
                    );
                    return (Some(Step { binds, force: None }), None);
                }
            };

            let callee = func.dfg.ext_funcs[fref].name.clone();
            let (callee_expr, call_kind) = resolve_callee(func, callee);

            match call_kind {
                CallKind::Pure => {
                    // Pure call: bind its (single) result.
                    let out = results[0];
                    let arg_exprs: Vec<Expr> = args
                        .iter()
                        .copied()
                        .map(|v| value_expr(func, v, st_name, vmctx))
                        .collect();
                    let e = Expr::app(callee_expr, arg_exprs);
                    let binds = vec![(value_name(out), e)];
                    (seq_on_result(binds), None)
                }
                CallKind::Indirect {
                    table_index,
                    type_index,
                } => {
                    let tmp = format!("call_{}", inst.as_u32());
                    let new_st = format!("st_{}", inst.as_u32());
                    let callee = args
                        .first()
                        .copied()
                        .map(|v| value_expr(func, v, st_name, vmctx))
                        .unwrap_or_else(|| Expr::int(0));
                    let wasm_args = args
                        .iter()
                        .copied()
                        .skip(1)
                        .filter(|v| *v != vmctx)
                        .map(|v| value_expr(func, v, st_name, vmctx))
                        .collect::<Vec<_>>();
                    let call_e = Expr::app(
                        callee_expr,
                        vec![
                            Expr::int(i64::from(table_index)),
                            Expr::int(i64::from(type_index)),
                            Expr::ident("moduleId"),
                            Expr::ident("funcs"),
                            Expr::ident("funcRegistry"),
                            callee,
                            Expr::list(wasm_args),
                            Expr::ident(st_name),
                        ],
                    );

                    let mut binds: Vec<(String, Expr)> = Vec::new();
                    binds.push((tmp.clone(), call_e));
                    binds.push((
                        new_st.clone(),
                        Expr::select(Expr::ident(&tmp), AttrKey::ident("st")),
                    ));

                    let results_list = Expr::select(Expr::ident(&tmp), AttrKey::ident("results"));
                    for (i, &rv) in results.iter().enumerate() {
                        let idx = Expr::int(i as i64);
                        let v = Expr::app(builtin_elem_at(), vec![results_list.clone(), idx]);
                        binds.push((value_name(rv), v));
                    }

                    (
                        Some(Step {
                            binds,
                            force: Some(Expr::ident(new_st.clone())),
                        }),
                        Some(new_st),
                    )
                }
                CallKind::StatefulRecord => {
                    let tmp = format!("call_{}", inst.as_u32());
                    let new_st = format!("st_{}", inst.as_u32());

                    let arg_exprs: Vec<Expr> = args
                        .iter()
                        .copied()
                        .map(|v| value_expr(func, v, st_name, vmctx))
                        .collect();
                    let call_e = Expr::app(callee_expr, arg_exprs);

                    let mut binds: Vec<(String, Expr)> = Vec::new();
                    binds.push((tmp.clone(), call_e));
                    binds.push((
                        new_st.clone(),
                        Expr::select(Expr::ident(&tmp), AttrKey::ident("st")),
                    ));

                    // results are in `tmp.results` as a list
                    let results_list = Expr::select(Expr::ident(&tmp), AttrKey::ident("results"));
                    for (i, &rv) in results.iter().enumerate() {
                        let idx = Expr::int(i as i64);
                        let v = Expr::app(builtin_elem_at(), vec![results_list.clone(), idx]);
                        binds.push((value_name(rv), v));
                    }

                    (
                        Some(Step {
                            binds,
                            force: Some(Expr::ident(new_st.clone())),
                        }),
                        Some(new_st),
                    )
                }
                CallKind::StatefulState => {
                    let new_st = format!("st_{}", inst.as_u32());
                    let arg_exprs: Vec<Expr> = args
                        .iter()
                        .copied()
                        .map(|v| value_expr(func, v, st_name, vmctx))
                        .collect();
                    let e = Expr::app(callee_expr, arg_exprs);
                    let binds = vec![(new_st.clone(), e)];
                    (
                        Some(Step {
                            binds,
                            force: Some(Expr::ident(new_st.clone())),
                        }),
                        Some(new_st),
                    )
                }
            }
        }

        // Default: unsupported instruction.
        _ => {
            let binds = unsupported_binds(
                inst,
                results,
                format!("wasm2nix: unimplemented inst opcode {op}"),
            );
            (Some(Step { binds, force: None }), None)
        }
    }
}

fn unsupported_binds(inst: Inst, results: &[ir::Value], message: String) -> Vec<(String, Expr)> {
    let throw_expr = || Expr::app(builtin_throw(), vec![Expr::string(message.clone())]);
    if results.is_empty() {
        vec![(format!("_unused_{}", inst.as_u32()), throw_expr())]
    } else {
        results
            .iter()
            .copied()
            .map(|value| (value_name(value), throw_expr()))
            .collect()
    }
}

fn lower_terminator(
    func: &ir::Function,
    inst: Inst,
    st_name: &str,
    vmctx: ir::Value,
    free_values: &BTreeMap<Block, Vec<ir::Value>>,
) -> Expr {
    let data = &func.dfg.insts[inst];
    let op = data.opcode();

    match (op, data) {
        (Opcode::Return, InstructionData::MultiAry { args, .. }) => {
            let rvs = args
                .as_slice(&func.dfg.value_lists)
                .iter()
                .copied()
                .map(|v| value_expr(func, v, st_name, vmctx))
                .collect::<Vec<_>>();
            Expr::attrset(
                false,
                vec![
                    (AttrKey::ident("st"), Expr::ident(st_name)),
                    (AttrKey::ident("results"), Expr::list(rvs)),
                ],
            )
        }

        (Opcode::Jump, InstructionData::Jump { destination, .. }) => {
            lower_block_call(func, *destination, st_name, vmctx, free_values)
        }

        (Opcode::Brif, InstructionData::Brif { blocks, arg, .. }) => {
            let cond = truthy_value_expr(func, *arg, st_name, vmctx);

            let then_call = lower_block_call(func, blocks[0], st_name, vmctx, free_values);
            let else_call = lower_block_call(func, blocks[1], st_name, vmctx, free_values);

            Expr::if_then_else(cond, then_call, else_call)
        }

        (Opcode::BrTable, InstructionData::BranchTable { arg, table, .. }) => {
            let index = value_expr(func, *arg, st_name, vmctx);
            let table_data = &func.dfg.jump_tables[*table];
            let mut expr = lower_block_call(
                func,
                table_data.default_block(),
                st_name,
                vmctx,
                free_values,
            );
            for (i, branch) in table_data.as_slice().iter().copied().enumerate().rev() {
                expr = Expr::if_then_else(
                    Expr::bin(BinOp::Eq, index.clone(), Expr::int(i as i64)),
                    lower_block_call(func, branch, st_name, vmctx, free_values),
                    expr,
                );
            }
            expr
        }

        (Opcode::Trap, _) => Expr::app(
            builtin_throw(),
            vec![Expr::string(format!(
                "wasm2nix: trap {}",
                data.trap_code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into())
            ))],
        ),

        (Opcode::Trapz, InstructionData::CondTrap { arg, code, .. }) => {
            let c = truthy_value_expr(func, *arg, st_name, vmctx);
            // trapz: if c is false then trap.
            Expr::if_then_else(
                c,
                Expr::attrset(
                    false,
                    vec![
                        (AttrKey::ident("st"), Expr::ident(st_name)),
                        (AttrKey::ident("results"), Expr::list(vec![])),
                    ],
                ),
                Expr::app(
                    builtin_throw(),
                    vec![Expr::string(format!("wasm2nix: trapz {code}"))],
                ),
            )
        }

        (Opcode::Trapnz, InstructionData::CondTrap { arg, code, .. }) => {
            let c = truthy_value_expr(func, *arg, st_name, vmctx);
            let ok = Expr::attrset(
                false,
                vec![
                    (AttrKey::ident("st"), Expr::ident(st_name)),
                    (AttrKey::ident("results"), Expr::list(vec![])),
                ],
            );
            // trapnz: if c is true then trap.
            Expr::if_then_else(
                c,
                Expr::app(
                    builtin_throw(),
                    vec![Expr::string(format!("wasm2nix: trapnz {code}"))],
                ),
                ok,
            )
        }

        _ => Expr::app(
            builtin_throw(),
            vec![Expr::string(format!(
                "wasm2nix: unsupported terminator {op}"
            ))],
        ),
    }
}

fn lower_block_call(
    func: &ir::Function,
    call: ir::BlockCall,
    st_name: &str,
    vmctx: ir::Value,
    free_values: &BTreeMap<Block, Vec<ir::Value>>,
) -> Expr {
    let target = call.block(&func.dfg.value_lists);
    let mut args = call
        .args_slice(&func.dfg.value_lists)
        .iter()
        .copied()
        .map(|v| value_expr(func, v, st_name, vmctx))
        .collect::<Vec<_>>();
    args.extend(
        free_values
            .get(&target)
            .into_iter()
            .flat_map(|values| values.iter())
            .map(|v| value_expr(func, *v, st_name, vmctx)),
    );
    args.push(Expr::ident(st_name));
    Expr::app(Expr::ident(block_name(target)), args)
}

#[derive(Debug, Clone, Copy)]
enum CallKind {
    Pure,
    StatefulRecord,
    StatefulState,
    Indirect { table_index: u32, type_index: u32 },
}

fn resolve_callee(func: &ir::Function, name: ir::ExternalName) -> (Expr, CallKind) {
    match name {
        ir::ExternalName::User(user_ref) => {
            let user = &func.params.user_named_funcs()[user_ref];
            match user.namespace {
                intr::FUNC_NAMESPACE => {
                    // wasm function: f{index}
                    (
                        Expr::ident(format!("f{}", user.index)),
                        CallKind::StatefulRecord,
                    )
                }
                intr::INTRINSIC_NAMESPACE => match user.index {
                    intr::MEM_GROW => (rt_fn(&["mem", "grow"]), CallKind::StatefulRecord),
                    intr::MEM_SIZE => (rt_fn(&["mem", "size"]), CallKind::Pure),
                    intr::MEM_COPY => (rt_fn(&["mem", "copy"]), CallKind::StatefulState),
                    intr::MEM_FILL => (rt_fn(&["mem", "fill"]), CallKind::StatefulState),
                    intr::MEM_INIT => (rt_fn(&["mem", "init"]), CallKind::StatefulState),
                    intr::DATA_DROP => (rt_fn(&["mem", "data_drop"]), CallKind::StatefulState),
                    intr::TABLE_COPY => (rt_fn(&["table", "copy"]), CallKind::StatefulState),
                    intr::TABLE_INIT => (rt_fn(&["table", "init"]), CallKind::StatefulState),
                    intr::ELEM_DROP => (rt_fn(&["table", "elem_drop"]), CallKind::StatefulState),
                    intr::TABLE_GET => (rt_fn(&["table", "get"]), CallKind::Pure),
                    intr::TABLE_SET => (rt_fn(&["table", "set"]), CallKind::StatefulState),

                    intr::GLOBAL_GET_I32 => (rt_fn(&["global", "get_i32"]), CallKind::Pure),
                    intr::GLOBAL_GET_I64 => (rt_fn(&["global", "get_i64"]), CallKind::Pure),
                    intr::GLOBAL_GET_F32 => (rt_fn(&["global", "get_f32"]), CallKind::Pure),
                    intr::GLOBAL_GET_F64 => (rt_fn(&["global", "get_f64"]), CallKind::Pure),

                    intr::GLOBAL_SET_I32 => {
                        (rt_fn(&["global", "set_i32"]), CallKind::StatefulState)
                    }
                    intr::GLOBAL_SET_I64 => {
                        (rt_fn(&["global", "set_i64"]), CallKind::StatefulState)
                    }
                    intr::GLOBAL_SET_F32 => {
                        (rt_fn(&["global", "set_f32"]), CallKind::StatefulState)
                    }
                    intr::GLOBAL_SET_F64 => {
                        (rt_fn(&["global", "set_f64"]), CallKind::StatefulState)
                    }

                    other if other >= intr::CALL_INDIRECT_BASE => {
                        let encoded = other - intr::CALL_INDIRECT_BASE;
                        let table_index = encoded / intr::CALL_INDIRECT_STRIDE;
                        let type_index = encoded % intr::CALL_INDIRECT_STRIDE;
                        (
                            rt_fn(&["table", "call"]),
                            CallKind::Indirect {
                                table_index,
                                type_index,
                            },
                        )
                    }

                    other => (
                        Expr::app(
                            builtin_throw(),
                            vec![Expr::string(format!("wasm2nix: unknown intrinsic {other}"))],
                        ),
                        CallKind::Pure,
                    ),
                },
                _ => (
                    Expr::app(
                        builtin_throw(),
                        vec![Expr::string(format!(
                            "wasm2nix: unknown external name namespace {}",
                            user.namespace
                        ))],
                    ),
                    CallKind::Pure,
                ),
            }
        }
        _ => (
            Expr::app(
                builtin_throw(),
                vec![Expr::string("wasm2nix: unsupported ExternalName")],
            ),
            CallKind::Pure,
        ),
    }
}

fn load_fn_name(op: Opcode, ty: ir::Type) -> &'static str {
    match op {
        Opcode::Load => match ty {
            I32 => "load_i32",
            I64 => "load_i64",
            F32 => "load_f32",
            F64 => "load_f64",
            _ => "load_unknown",
        },
        Opcode::Uload8 => "load_u8",
        Opcode::Sload8 if ty == I64 => "load_s8_i64",
        Opcode::Sload8 => "load_s8",
        Opcode::Uload16 => "load_u16",
        Opcode::Sload16 if ty == I64 => "load_s16_i64",
        Opcode::Sload16 => "load_s16",
        Opcode::Uload32 => "load_u32",
        Opcode::Sload32 if ty == I64 => "load_s32_i64",
        Opcode::Sload32 => "load_s32",
        _ => "load_unknown",
    }
}

fn store_fn_name(op: Opcode, ty: ir::Type) -> &'static str {
    match op {
        Opcode::Store => match ty {
            I32 => "store_i32",
            I64 => "store_i64",
            F32 => "store_f32",
            F64 => "store_f64",
            _ => "store_unknown",
        },
        Opcode::Istore8 => "store_u8",
        Opcode::Istore16 => "store_u16",
        Opcode::Istore32 => "store_i32",
        _ => "store_unknown",
    }
}

fn intcc_name(cc: IntCC) -> &'static str {
    match cc {
        IntCC::Equal => "eq",
        IntCC::NotEqual => "ne",
        IntCC::SignedLessThan => "lt_s",
        IntCC::SignedGreaterThanOrEqual => "ge_s",
        IntCC::SignedGreaterThan => "gt_s",
        IntCC::SignedLessThanOrEqual => "le_s",
        IntCC::UnsignedLessThan => "lt_u",
        IntCC::UnsignedGreaterThanOrEqual => "ge_u",
        IntCC::UnsignedGreaterThan => "gt_u",
        IntCC::UnsignedLessThanOrEqual => "le_u",
    }
}

fn floatcc_name(cc: FloatCC) -> &'static str {
    match cc {
        FloatCC::Ordered => "ord",
        FloatCC::Unordered => "uno",
        FloatCC::Equal => "eq",
        FloatCC::NotEqual => "ne",
        FloatCC::OrderedNotEqual => "one",
        FloatCC::UnorderedOrEqual => "ueq",
        FloatCC::LessThan => "lt",
        FloatCC::LessThanOrEqual => "le",
        FloatCC::GreaterThan => "gt",
        FloatCC::GreaterThanOrEqual => "ge",
        FloatCC::UnorderedOrLessThan => "ult",
        FloatCC::UnorderedOrLessThanOrEqual => "ule",
        FloatCC::UnorderedOrGreaterThan => "ugt",
        FloatCC::UnorderedOrGreaterThanOrEqual => "uge",
    }
}

fn value_name(v: ir::Value) -> String {
    format!("v{}", v.as_u32())
}

fn block_name(b: Block) -> String {
    format!("b{}", b.as_u32())
}

fn value_expr(func: &ir::Function, v: ir::Value, st_name: &str, vmctx: ir::Value) -> Expr {
    let v = func.dfg.resolve_aliases(v);
    if v == vmctx {
        Expr::ident(st_name)
    } else {
        Expr::ident(value_name(v))
    }
}

fn truthy_value_expr(func: &ir::Function, v: ir::Value, st_name: &str, vmctx: ir::Value) -> Expr {
    let value = value_expr(func, v, st_name, vmctx);
    if func.dfg.value_type(v) == I8 {
        value
    } else {
        Expr::bin(BinOp::Neq, value, Expr::int(0))
    }
}

fn lambda_chain(params: &[String], body: Expr) -> Expr {
    params
        .iter()
        .rev()
        .fold(body, |acc, p| Expr::lambda(p.clone(), acc))
}

fn builtin_seq() -> Expr {
    Expr::select(Expr::ident("builtins"), AttrKey::ident("seq"))
}

fn builtin_elem_at() -> Expr {
    Expr::select(Expr::ident("builtins"), AttrKey::ident("elemAt"))
}

fn builtin_throw() -> Expr {
    Expr::ident("throw")
}

fn rt_fn(path: &[&str]) -> Expr {
    let mut e = Expr::ident("rt");
    for p in path {
        e = Expr::select(e, AttrKey::ident(*p));
    }
    e
}

fn global_get_name(ty: WasmValType) -> &'static str {
    match ty {
        WasmValType::I32 => "get_i32",
        WasmValType::I64 => "get_i64",
        WasmValType::F32 => "get_f32",
        WasmValType::F64 => "get_f64",
        WasmValType::V128 | WasmValType::Ref(_) => "get_i32",
    }
}

fn global_set_name(ty: WasmValType) -> &'static str {
    match ty {
        WasmValType::I32 => "set_i32",
        WasmValType::I64 => "set_i64",
        WasmValType::F32 => "set_f32",
        WasmValType::F64 => "set_f64",
        WasmValType::V128 | WasmValType::Ref(_) => "set_i32",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translator::NixModule;

    fn lower_wasm(wasm: &[u8]) -> String {
        let module = NixModule::translate(wasm).expect("wasm should translate");
        lower_module(&module).render()
    }

    #[test]
    fn generated_module_keeps_compat_exports_and_instantiate() {
        let wasm = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x07, 0x01, 0x60, 0x02, 0x7f,
            0x7f, 0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64,
            0x00, 0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b,
        ];
        let nix = lower_wasm(&wasm);

        assert!(nix.contains("\"add\" = funcs.f0;"));
        assert!(nix.contains("instantiate = _args:"));
        assert!(nix.contains("st = st;"));
        assert!(nix.contains("exports = exports;"));
        assert!(nix.contains("__wasm2nix ="));
    }

    #[test]
    fn generated_module_embeds_active_data_segments() {
        let wasm = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01,
            0x7f, 0x03, 0x02, 0x01, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07, 0x11, 0x02, 0x06,
            0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00, 0x04, 0x6c, 0x6f, 0x61, 0x64, 0x00,
            0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x41, 0x04, 0x28, 0x02, 0x00, 0x0b, 0x0b, 0x0a,
            0x01, 0x00, 0x41, 0x04, 0x0b, 0x04, 0x01, 0x02, 0x03, 0x04,
        ];
        let nix = lower_wasm(&wasm);

        assert!(nix.contains("pageCount = 1;"));
        assert!(nix.contains("\"1\" = 67305985;"));
        assert!(nix.contains("\"memory\" = rt.mem;"));
    }

    #[test]
    fn generated_module_runs_start_before_exposing_instance_state() {
        let wasm = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00, 0x06, 0x06, 0x01, 0x7f, 0x01, 0x41, 0x00, 0x0b, 0x07, 0x05,
            0x01, 0x01, 0x67, 0x03, 0x00, 0x08, 0x01, 0x00, 0x0a, 0x08, 0x01, 0x06, 0x00, 0x41,
            0x2a, 0x24, 0x00, 0x0b,
        ];
        let nix = lower_wasm(&wasm);

        assert!(nix.contains("\"0\" = 0;"));
        assert!(nix.contains("st = (funcs.f0 st0).st;"));
        assert!(nix.contains("rt.global.set_i32"));
        assert!(nix.contains("\"g\" = {"));
    }

    #[test]
    fn generated_module_lowers_call_indirect_through_table_runtime() {
        let wasm = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f,
            0x01, 0x7f, 0x03, 0x03, 0x02, 0x00, 0x00, 0x04, 0x04, 0x01, 0x70, 0x00, 0x01, 0x07,
            0x08, 0x01, 0x04, 0x63, 0x61, 0x6c, 0x6c, 0x00, 0x01, 0x09, 0x07, 0x01, 0x00, 0x41,
            0x00, 0x0b, 0x01, 0x00, 0x0a, 0x13, 0x02, 0x07, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6a,
            0x0b, 0x09, 0x00, 0x20, 0x00, 0x41, 0x00, 0x11, 0x00, 0x00, 0x0b,
        ];
        let nix = lower_wasm(&wasm);

        assert!(nix.contains("rt.table.call 0 0 moduleId funcs funcRegistry"));
        assert!(nix.contains("tables = {"));
        assert!(nix.contains("module = moduleId;"));
        assert!(nix.contains("func = 0;"));
        assert!(nix.contains("type = 0;"));
    }
}
