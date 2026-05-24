use std::collections::BTreeMap;

use cranelift_codegen::{
    ir,
    ir::{
        ArgumentPurpose, Block, Inst, InstructionData, Opcode,
        condcodes::{FloatCC, IntCC},
        types::{I8, I16, I32, I64},
    },
};
use snafu::prelude::*;

use super::{
    builtin_elem_at, builtin_foldl_strict, builtin_generic_closure, builtin_get_attr,
    builtin_length, builtin_seq, builtin_throw,
    error::{
        EmptyBlockSnafu, MissingEntryBlockSnafu, MissingTypeSnafu, MissingVmContextSnafu, Result,
        UnknownExternalNamespaceSnafu, UnknownIntrinsicSnafu, UnsupportedExternalNameSnafu,
        UnsupportedOpcodeSnafu,
    },
    lambda_chain, lower_table_entry, rt_fn, signature_id, types,
};
use crate::{
    intrinsics as intr,
    nix::{AttrKey, BinOp, Expr, UnOp},
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

    let free_values = block_free_values(func, vmctx);

    // Blocks return either a jump record or a done record. The trampoline below
    // keeps Wasm loops from consuming the Nix evaluator call stack.
    let mut block_binds: Vec<(AttrKey, Expr)> = Vec::new();
    for block in func.layout.blocks() {
        let bname = block_name(block);
        let bexpr = lower_block(module, func, block, vmctx, &free_values)?;
        block_binds.push((AttrKey::ident(bname), bexpr));
    }
    let blocks_attrset = Expr::attrset(true, block_binds);

    let mut entry_args: Vec<Expr> = func
        .dfg
        .block_params(entry)
        .iter()
        .copied()
        .filter(|v| *v != vmctx)
        .map(|v| Expr::ident(value_name(v)))
        .collect();
    entry_args.extend(
        free_values
            .get(&entry)
            .into_iter()
            .flat_map(|values| values.iter())
            .map(|v| Expr::ident(value_name(*v))),
    );

    let entry_with_key = lower_jump_record_with_key(entry, entry_args, Expr::ident("st"), 0);
    let apply_args = Expr::lambda(
        "fn",
        Expr::lambda(
            "args",
            Expr::app(
                builtin_foldl_strict(),
                vec![
                    Expr::lambda(
                        "f",
                        Expr::lambda("arg", Expr::app(Expr::ident("f"), vec![Expr::ident("arg")])),
                    ),
                    Expr::ident("fn"),
                    Expr::ident("args"),
                ],
            ),
        ),
    );
    let apply_block = Expr::lambda(
        "block",
        Expr::lambda(
            "args",
            Expr::lambda(
                "blockSt",
                Expr::app(
                    Expr::app(
                        Expr::ident("applyArgs"),
                        vec![
                            Expr::app(
                                builtin_get_attr(),
                                vec![Expr::ident("block"), Expr::ident("blocks")],
                            ),
                            Expr::ident("args"),
                        ],
                    ),
                    vec![Expr::ident("blockSt")],
                ),
            ),
        ),
    );
    let step_item = Expr::lambda(
        "current",
        Expr::if_then_else(
            Expr::bin(
                BinOp::Eq,
                Expr::select(Expr::ident("current"), AttrKey::ident("tag")),
                Expr::string("done"),
            ),
            Expr::list(vec![]),
            Expr::list(vec![Expr::app(
                builtin_seq(),
                vec![
                    Expr::bool(true),
                    Expr::let_in(
                        vec![(
                            "next".into(),
                            Expr::app(
                                Expr::ident("applyBlock"),
                                vec![
                                    Expr::select(Expr::ident("current"), AttrKey::ident("block")),
                                    Expr::select(Expr::ident("current"), AttrKey::ident("args")),
                                    Expr::select(Expr::ident("current"), AttrKey::ident("st")),
                                ],
                            ),
                        )],
                        Expr::bin(
                            BinOp::Update,
                            Expr::ident("next"),
                            Expr::attrset(
                                false,
                                vec![(
                                    AttrKey::ident("key"),
                                    Expr::bin(
                                        BinOp::Add,
                                        Expr::select(Expr::ident("current"), AttrKey::ident("key")),
                                        Expr::int(1),
                                    ),
                                )],
                            ),
                        ),
                    ),
                ],
            )]),
        ),
    );
    let trace_expr = Expr::app(
        builtin_generic_closure(),
        vec![Expr::attrset(
            false,
            vec![
                (AttrKey::ident("startSet"), Expr::list(vec![entry_with_key])),
                (AttrKey::ident("operator"), Expr::ident("stepItem")),
            ],
        )],
    );
    let result_index = Expr::bin(
        BinOp::Sub,
        Expr::app(builtin_length(), vec![Expr::ident("trace")]),
        Expr::int(1),
    );
    let return_result = Expr::attrset(
        false,
        vec![
            (
                AttrKey::ident("st"),
                Expr::select(Expr::ident("result"), AttrKey::ident("st")),
            ),
            (
                AttrKey::ident("results"),
                Expr::select(Expr::ident("result"), AttrKey::ident("results")),
            ),
        ],
    );
    let body = Expr::let_in(
        vec![
            ("blocks".into(), blocks_attrset),
            ("applyArgs".into(), apply_args),
            ("applyBlock".into(), apply_block),
            ("stepItem".into(), step_item),
            ("trace".into(), trace_expr),
            (
                "result".into(),
                Expr::app(builtin_elem_at(), vec![Expr::ident("trace"), result_index]),
            ),
        ],
        return_result,
    );
    Ok(lambda_chain(&params, body))
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

fn push_free_value(
    func: &ir::Function, value: ir::Value, vmctx: ir::Value, defined: &[ir::Value],
    free: &mut Vec<ir::Value>,
) {
    let value = func.dfg.resolve_aliases(value);
    if value != vmctx && !defined.contains(&value) && !free.contains(&value) {
        free.push(value);
    }
}

fn lower_block(
    module: &NixModule, func: &ir::Function, block: Block, vmctx: ir::Value,
    free_values: &BTreeMap<Block, Vec<ir::Value>>,
) -> Result<Expr> {
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

    let body = lower_block_body(module, func, block, vmctx, free_values)?;
    Ok(lambda_chain(&params, body))
}

struct Step {
    binds: Vec<(String, Expr)>,
    force: Option<Expr>,
}

fn lower_block_body(
    module: &NixModule, func: &ir::Function, block: Block, vmctx: ir::Value,
    free_values: &BTreeMap<Block, Vec<ir::Value>>,
) -> Result<Expr> {
    let mut insts: Vec<Inst> = func.layout.block_insts(block).collect();
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
        let (step, new_st) = lower_inst(module, func, inst, &st_name, vmctx)?;
        if let Some(step) = step {
            steps.push(step);
        }
        if let Some(new_st) = new_st {
            st_name = new_st;
        }
    }

    let mut expr = lower_terminator(func, term, &st_name, vmctx, free_values)?;
    for step in steps.into_iter().rev() {
        let body = if let Some(force) = step.force {
            Expr::app(builtin_seq(), vec![force, expr])
        } else {
            expr
        };
        expr = Expr::let_in(step.binds, body);
    }

    Ok(expr)
}

fn lower_inst(
    module: &NixModule, func: &ir::Function, inst: Inst, st_name: &str, vmctx: ir::Value,
) -> Result<(Option<Step>, Option<String>)> {
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
        return Ok((seq_on_result(binds), None));
    }

    Ok(match data {
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
                    let float_key = types::float_type_key(in_ty, "float-to-int input")?;
                    let int_key = types::int_type_key(out_ty, "float-to-int output")?;
                    let conv_name = match op {
                        Opcode::FcvtToSint => "trunc_s",
                        Opcode::FcvtToUint => "trunc_u",
                        Opcode::FcvtToSintSat => "trunc_sat_s",
                        Opcode::FcvtToUintSat => "trunc_sat_u",
                        _ => {
                            return UnsupportedOpcodeSnafu {
                                opcode: op.to_string(),
                            }
                            .fail();
                        }
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
                    let int_key = types::int_type_key(in_ty, "int-to-float input")?;
                    let float_key = types::float_type_key(out_ty, "int-to-float output")?;
                    let conv_name = match op {
                        Opcode::FcvtFromSint => "convert_s",
                        Opcode::FcvtFromUint => "convert_u",
                        _ => {
                            return UnsupportedOpcodeSnafu {
                                opcode: op.to_string(),
                            }
                            .fail();
                        }
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
                    let ty_key = types::int_type_key(out_ty, "integer unary output")?;
                    let op_name = match op {
                        Opcode::Clz => "clz",
                        Opcode::Ctz => "ctz",
                        Opcode::Popcnt => "popcnt",
                        _ => {
                            return UnsupportedOpcodeSnafu {
                                opcode: op.to_string(),
                            }
                            .fail();
                        }
                    };
                    let binds = vec![(
                        value_name(out),
                        Expr::app(rt_fn(&[ty_key, op_name]), vec![a]),
                    )];
                    (seq_on_result(binds), None)
                }
                Opcode::Fneg
                | Opcode::Fabs
                | Opcode::Sqrt
                | Opcode::Ceil
                | Opcode::Floor
                | Opcode::Trunc
                | Opcode::Nearest => {
                    let out = results[0];
                    let a = value_expr(func, *arg, st_name, vmctx);
                    let out_ty = func.dfg.value_type(out);
                    let ty_key = types::float_type_key(out_ty, "float unary output")?;
                    let op_name = match op {
                        Opcode::Fneg => "neg",
                        Opcode::Fabs => "abs",
                        Opcode::Sqrt => "sqrt",
                        Opcode::Ceil => "ceil",
                        Opcode::Floor => "floor",
                        Opcode::Trunc => "trunc",
                        Opcode::Nearest => "nearest",
                        _ => {
                            return UnsupportedOpcodeSnafu {
                                opcode: op.to_string(),
                            }
                            .fail();
                        }
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
            if out_ty == I8 {
                let e = match op {
                    Opcode::Band => Expr::bin(BinOp::And, a, b),
                    Opcode::Bor => Expr::bin(BinOp::Or, a, b),
                    Opcode::Bxor => Expr::bin(BinOp::Neq, a, b),
                    _ => {
                        return UnsupportedOpcodeSnafu {
                            opcode: op.to_string(),
                        }
                        .fail();
                    }
                };
                let binds = vec![(value_name(out), e)];
                return Ok((seq_on_result(binds), None));
            }
            let ty_key = types::numeric_type_key(out_ty, "binary output")?;

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
                _ => {
                    return UnsupportedOpcodeSnafu {
                        opcode: op.to_string(),
                    }
                    .fail();
                }
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

            let ty_key = types::int_type_key(out_ty, "binary immediate output")?;
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
                    return Ok((Some(Step { binds, force: None }), None));
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
            let float_ns = types::float_type_key(ty, "float comparison input")?;
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
            let e = Expr::app(
                rt_fn(&["mem", types::load_fn_name(op, out_ty)?]),
                vec![addr, st],
            );
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
                rt_fn(&[
                    "mem",
                    types::store_fn_name(op, func.dfg.value_type(args[0]))?,
                ]),
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
            let trap_name = format!("trap_{}", inst.as_u32());
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
            let trap_name = format!("trap_{}", inst.as_u32());
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
                    return Ok((Some(Step { binds, force: None }), None));
                }
            };

            let callee = func.dfg.ext_funcs[fref].name.clone();
            let (callee_expr, call_kind) = resolve_callee(module, func, callee)?;

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
                    type_id,
                } => {
                    let tmp = format!("call_{}", inst.as_u32());
                    let new_st = format!("st_{}", inst.as_u32());
                    let callee = match args.first().copied() {
                        Some(value) => value_expr(func, value, st_name, vmctx),
                        None => Expr::int(0),
                    };
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
                            Expr::string(type_id),
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
    })
}

fn unsupported_binds(inst: Inst, results: &[ir::Value], message: String) -> Vec<(String, Expr)> {
    let throw_expr = || Expr::app(builtin_throw(), vec![Expr::string(message.clone())]);
    if results.is_empty() {
        vec![(format!("unsupported_{}", inst.as_u32()), throw_expr())]
    } else {
        results
            .iter()
            .copied()
            .map(|value| (value_name(value), throw_expr()))
            .collect()
    }
}

fn lower_terminator(
    func: &ir::Function, inst: Inst, st_name: &str, vmctx: ir::Value,
    free_values: &BTreeMap<Block, Vec<ir::Value>>,
) -> Result<Expr> {
    let data = &func.dfg.insts[inst];
    let op = data.opcode();

    Ok(match (op, data) {
        (Opcode::Return, InstructionData::MultiAry { args, .. }) => {
            let rvs = args
                .as_slice(&func.dfg.value_lists)
                .iter()
                .copied()
                .map(|v| value_expr(func, v, st_name, vmctx))
                .collect::<Vec<_>>();
            lower_done_record(Expr::ident(st_name), rvs)
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

        (Opcode::Trap, _) => {
            let trap_code = match data.trap_code() {
                Some(code) => code.to_string(),
                None => "?".into(),
            };
            Expr::app(
                builtin_throw(),
                vec![Expr::string(format!("wasm2nix: trap {trap_code}"))],
            )
        }

        (Opcode::Trapz, InstructionData::CondTrap { arg, code, .. }) => {
            let c = truthy_value_expr(func, *arg, st_name, vmctx);
            // trapz: if c is false then trap.
            Expr::if_then_else(
                c,
                lower_done_record(Expr::ident(st_name), vec![]),
                Expr::app(
                    builtin_throw(),
                    vec![Expr::string(format!("wasm2nix: trapz {code}"))],
                ),
            )
        }

        (Opcode::Trapnz, InstructionData::CondTrap { arg, code, .. }) => {
            let c = truthy_value_expr(func, *arg, st_name, vmctx);
            let ok = lower_done_record(Expr::ident(st_name), vec![]);
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

        _ => {
            return UnsupportedOpcodeSnafu {
                opcode: op.to_string(),
            }
            .fail();
        }
    })
}

fn lower_block_call(
    func: &ir::Function, call: ir::BlockCall, st_name: &str, vmctx: ir::Value,
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
    lower_jump_record(target, args, Expr::ident(st_name))
}

fn lower_jump_record(target: Block, args: Vec<Expr>, st: Expr) -> Expr {
    lower_jump_record_binds(target, args, st, None)
}

fn lower_jump_record_with_key(target: Block, args: Vec<Expr>, st: Expr, key: i64) -> Expr {
    lower_jump_record_binds(target, args, st, Some(key))
}

fn lower_jump_record_binds(target: Block, args: Vec<Expr>, st: Expr, key: Option<i64>) -> Expr {
    let mut binds = vec![
        (AttrKey::ident("tag"), Expr::string("jump")),
        (AttrKey::ident("block"), Expr::string(block_name(target))),
        (AttrKey::ident("args"), Expr::list(args)),
        (AttrKey::ident("st"), st),
    ];
    if let Some(key) = key {
        binds.push((AttrKey::ident("key"), Expr::int(key)));
    }
    Expr::attrset(false, binds)
}

fn lower_done_record(st: Expr, results: Vec<Expr>) -> Expr {
    Expr::attrset(
        false,
        vec![
            (AttrKey::ident("tag"), Expr::string("done")),
            (AttrKey::ident("st"), st),
            (AttrKey::ident("results"), Expr::list(results)),
        ],
    )
}

#[derive(Debug, Clone)]
enum CallKind {
    Pure,
    StatefulRecord,
    StatefulState,
    Indirect { table_index: u32, type_id: String },
}

fn resolve_callee(
    module: &NixModule, func: &ir::Function, name: ir::ExternalName,
) -> Result<(Expr, CallKind)> {
    match name {
        ir::ExternalName::User(user_ref) => {
            let user = &func.params.user_named_funcs()[user_ref];
            match intr::UserNamespace::try_from(user.namespace) {
                Ok(intr::UserNamespace::Func) => {
                    // wasm function: f{index}
                    Ok((
                        Expr::ident(format!("f{}", user.index)),
                        CallKind::StatefulRecord,
                    ))
                }
                Ok(intr::UserNamespace::Intrinsic) => match intr::Intrinsic::try_from(user.index) {
                    Ok(intr::Intrinsic::MemGrow) => {
                        Ok((rt_fn(&["mem", "grow"]), CallKind::StatefulRecord))
                    }
                    Ok(intr::Intrinsic::MemSize) => Ok((rt_fn(&["mem", "size"]), CallKind::Pure)),
                    Ok(intr::Intrinsic::MemCopy) => {
                        Ok((rt_fn(&["mem", "copy"]), CallKind::StatefulState))
                    }
                    Ok(intr::Intrinsic::MemFill) => {
                        Ok((rt_fn(&["mem", "fill"]), CallKind::StatefulState))
                    }
                    Ok(intr::Intrinsic::MemInit) => {
                        Ok((rt_fn(&["mem", "init"]), CallKind::StatefulState))
                    }
                    Ok(intr::Intrinsic::DataDrop) => {
                        Ok((rt_fn(&["mem", "data_drop"]), CallKind::StatefulState))
                    }
                    Ok(intr::Intrinsic::TableCopy) => {
                        Ok((rt_fn(&["table", "copy"]), CallKind::StatefulState))
                    }
                    Ok(intr::Intrinsic::TableInit) => {
                        Ok((rt_fn(&["table", "init"]), CallKind::StatefulState))
                    }
                    Ok(intr::Intrinsic::ElemDrop) => {
                        Ok((rt_fn(&["table", "elem_drop"]), CallKind::StatefulState))
                    }
                    Ok(intr::Intrinsic::TableGet) => Ok((rt_fn(&["table", "get"]), CallKind::Pure)),
                    Ok(intr::Intrinsic::TableSet) => {
                        Ok((rt_fn(&["table", "set"]), CallKind::StatefulState))
                    }
                    Ok(intr::Intrinsic::TableFill) => {
                        Ok((rt_fn(&["table", "fill"]), CallKind::StatefulState))
                    }
                    Ok(intr::Intrinsic::TableSize) => {
                        Ok((rt_fn(&["table", "size"]), CallKind::Pure))
                    }
                    Ok(intr::Intrinsic::TableGrow) => {
                        Ok((rt_fn(&["table", "grow"]), CallKind::StatefulRecord))
                    }
                    Ok(intr::Intrinsic::GlobalGetI32) => {
                        Ok((rt_fn(&["global", "get_i32"]), CallKind::Pure))
                    }
                    Ok(intr::Intrinsic::GlobalGetI64) => {
                        Ok((rt_fn(&["global", "get_i64"]), CallKind::Pure))
                    }
                    Ok(intr::Intrinsic::GlobalGetF32) => {
                        Ok((rt_fn(&["global", "get_f32"]), CallKind::Pure))
                    }
                    Ok(intr::Intrinsic::GlobalGetF64) => {
                        Ok((rt_fn(&["global", "get_f64"]), CallKind::Pure))
                    }
                    Ok(intr::Intrinsic::GlobalSetI32) => {
                        Ok((rt_fn(&["global", "set_i32"]), CallKind::StatefulState))
                    }
                    Ok(intr::Intrinsic::GlobalSetI64) => {
                        Ok((rt_fn(&["global", "set_i64"]), CallKind::StatefulState))
                    }
                    Ok(intr::Intrinsic::GlobalSetF32) => {
                        Ok((rt_fn(&["global", "set_f32"]), CallKind::StatefulState))
                    }
                    Ok(intr::Intrinsic::GlobalSetF64) => {
                        Ok((rt_fn(&["global", "set_f64"]), CallKind::StatefulState))
                    }
                    Ok(intr::Intrinsic::RefFunc { func_index }) => {
                        Ok((lower_table_entry(module, func_index), CallKind::Pure))
                    }
                    Ok(intr::Intrinsic::CallIndirect {
                        table_index,
                        type_index,
                    }) => {
                        let sig_index = cranelift_wasm::TypeIndex::from_u32(type_index);
                        let Some(sig) = module.info.signatures.get(sig_index) else {
                            return MissingTypeSnafu { type_index }.fail();
                        };
                        Ok((
                            rt_fn(&["table", "call"]),
                            CallKind::Indirect {
                                table_index,
                                type_id: signature_id(sig),
                            },
                        ))
                    }
                    Err(_) => UnknownIntrinsicSnafu { index: user.index }.fail(),
                },
                Ok(intr::UserNamespace::Symbol) | Err(_) => UnknownExternalNamespaceSnafu {
                    namespace: user.namespace,
                }
                .fail(),
            }
        }
        other => UnsupportedExternalNameSnafu {
            name: format!("{other:?}"),
        }
        .fail(),
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
