use crate::nix::{AttrKey, BinOp, Expr, UnOp};
use crate::translator::NixModule;
use cranelift_codegen::ir;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types::{F32, F64, I8, I32, I64};
use cranelift_codegen::ir::{ArgumentPurpose, Block, Inst, InstructionData, Opcode};

const FUNC_NAMESPACE: u32 = 0;
const INTRINSIC_NAMESPACE: u32 = 1;

// Must match `translator.rs`
const INTR_MEM_GROW: u32 = 0;
const INTR_MEM_SIZE: u32 = 1;
const INTR_GLOBAL_GET_I32: u32 = 10;
const INTR_GLOBAL_GET_I64: u32 = 11;
const INTR_GLOBAL_GET_F32: u32 = 12;
const INTR_GLOBAL_GET_F64: u32 = 13;
const INTR_GLOBAL_SET_I32: u32 = 20;
const INTR_GLOBAL_SET_I64: u32 = 21;
const INTR_GLOBAL_SET_F32: u32 = 22;
const INTR_GLOBAL_SET_F64: u32 = 23;

pub fn lower_module(module: &NixModule) -> Expr {
    // Top-level: rt: imports: let funcs = rec { ... }; in { ...exports... }
    let funcs_expr = lower_funcs_rec_attrset(module);
    let exports_expr = lower_exports_attrset(module);

    Expr::lambda(
        "rt",
        Expr::lambda(
            "imports",
            Expr::let_in(vec![("funcs".into(), funcs_expr)], exports_expr),
        ),
    )
}

fn lower_funcs_rec_attrset(module: &NixModule) -> Expr {
    let mut binds: Vec<(AttrKey, Expr)> = Vec::new();

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

    Expr::attrset(false, binds)
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

    // Blocks: `blocks = rec { b0 = ...; b1 = ...; }; in blocks.b0 <args> st`
    let mut block_binds: Vec<(AttrKey, Expr)> = Vec::new();
    for block in func.layout.blocks() {
        let bname = block_name(block);
        let bexpr = lower_block(func, block, vmctx);
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
        args.push(Expr::ident("st"));
        Expr::app(b0, args)
    };

    let body = Expr::let_in(vec![("blocks".into(), blocks_attrset)], entry_call);
    lambda_chain(&params, body)
}

fn lower_block(func: &ir::Function, block: Block, vmctx: ir::Value) -> Expr {
    // Block parameters: block params excluding vmctx, then `st`.
    let mut params: Vec<String> = func
        .dfg
        .block_params(block)
        .iter()
        .copied()
        .filter(|v| *v != vmctx)
        .map(value_name)
        .collect();
    params.push("st".into());

    let body = lower_block_body(func, block, vmctx);
    lambda_chain(&params, body)
}

struct Step {
    binds: Vec<(String, Expr)>,
    force: Option<Expr>,
}

fn lower_block_body(func: &ir::Function, block: Block, vmctx: ir::Value) -> Expr {
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

    let mut expr = lower_terminator(func, term, &st_name, vmctx);
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
                    let a = value_expr(*arg, st_name, vmctx);

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
                    let a = value_expr(*arg, st_name, vmctx);
                    let binds = vec![(
                        value_name(out),
                        Expr::app(rt_fn(&["conv", "sextend"]), vec![a]),
                    )];
                    (seq_on_result(binds), None)
                }
                Opcode::Ireduce => {
                    let out = results[0];
                    let a = value_expr(*arg, st_name, vmctx);
                    let binds = vec![(
                        value_name(out),
                        Expr::app(rt_fn(&["conv", "ireduce"]), vec![a]),
                    )];
                    (seq_on_result(binds), None)
                }
                Opcode::Bitcast => {
                    let out = results[0];
                    // With our representation, bitcast between i32<->f32 and i64<->f64 is identity.
                    let a = value_expr(*arg, st_name, vmctx);
                    let binds = vec![(value_name(out), a)];
                    (seq_on_result(binds), None)
                }
                Opcode::Bnot => {
                    let out = results[0];
                    let a = value_expr(*arg, st_name, vmctx);
                    let binds =
                        vec![(value_name(out), Expr::app(rt_fn(&["int", "bnot"]), vec![a]))];
                    (seq_on_result(binds), None)
                }
                Opcode::Ineg => {
                    let out = results[0];
                    let a = value_expr(*arg, st_name, vmctx);
                    let binds = vec![(value_name(out), Expr::un(UnOp::Neg, a))];
                    (seq_on_result(binds), None)
                }
                _ => {
                    let binds = vec![(
                        format!("_unused_{}", inst.as_u32()),
                        Expr::app(
                            builtin_throw(),
                            vec![Expr::string(format!(
                                "wasm2nix: unimplemented unary op {op}"
                            ))],
                        ),
                    )];
                    (Some(Step { binds, force: None }), None)
                }
            }
        }

        InstructionData::Binary {
            args: [arg1, arg2], ..
        } => {
            let a = value_expr(*arg1, st_name, vmctx);
            let b = value_expr(*arg2, st_name, vmctx);
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
                _ => unimplemented!("wasm2nix: unimplemented binary op {op:?}"),
            };

            let e = Expr::app(rt_fn(&[ty_key, op_name]), vec![a, b]);
            let binds = vec![(value_name(out), e)];
            (seq_on_result(binds), None)
        }

        InstructionData::BinaryImm64 { arg, imm, .. } => {
            let a = value_expr(*arg, st_name, vmctx);
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
                    let binds = vec![(
                        format!("_unused_{}", inst.as_u32()),
                        Expr::app(
                            builtin_throw(),
                            vec![Expr::string(format!(
                                "wasm2nix: unimplemented binary-imm op {op}"
                            ))],
                        ),
                    )];
                    return (Some(Step { binds, force: None }), None);
                }
            };

            let e = Expr::app(rt_fn(&[ty_key, op_name]), vec![a, imm_expr]);
            let binds = vec![(value_name(out), e)];
            (seq_on_result(binds), None)
        }

        InstructionData::IntCompare { args, cond, .. } if op == Opcode::Icmp => {
            let out = results[0];
            let a = value_expr(args[0], st_name, vmctx);
            let b = value_expr(args[1], st_name, vmctx);
            let e = Expr::app(rt_fn(&["cmp", intcc_name(*cond)]), vec![a, b]);
            let binds = vec![(value_name(out), e)];
            (seq_on_result(binds), None)
        }

        InstructionData::IntCompareImm { arg, cond, imm, .. } if op == Opcode::IcmpImm => {
            let out = results[0];
            let a = value_expr(*arg, st_name, vmctx);
            let ty = func.dfg.value_type(*arg);
            let imm64 = i64::from(*imm);
            let imm_expr = if ty == I32 {
                Expr::int(i64::from(imm64 as u32))
            } else {
                Expr::int(imm64)
            };
            let e = Expr::app(rt_fn(&["cmp", intcc_name(*cond)]), vec![a, imm_expr]);
            let binds = vec![(value_name(out), e)];
            (seq_on_result(binds), None)
        }

        InstructionData::Load { arg, offset, .. } => {
            let out = results[0];
            let out_ty = func.dfg.value_type(out);
            let base = value_expr(*arg, st_name, vmctx);
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
            let val = value_expr(args[0], st_name, vmctx);
            let base = value_expr(args[1], st_name, vmctx);
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

        // Calls: use analyze_call() to get func_ref and args.
        _ if op == Opcode::Call => {
            let call_info = data.analyze_call(&func.dfg.value_lists);
            let (fref, args) = match call_info {
                ir::instructions::CallInfo::Direct(fref, args) => (fref, args),
                _ => {
                    let binds = vec![(
                        format!("_unused_{}", inst.as_u32()),
                        Expr::app(
                            builtin_throw(),
                            vec![Expr::string("wasm2nix: unsupported call kind")],
                        ),
                    )];
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
                        .map(|v| value_expr(v, st_name, vmctx))
                        .collect();
                    let e = Expr::app(callee_expr, arg_exprs);
                    let binds = vec![(value_name(out), e)];
                    (seq_on_result(binds), None)
                }
                CallKind::StatefulRecord => {
                    let tmp = format!("call_{}", inst.as_u32());
                    let new_st = format!("st_{}", inst.as_u32());

                    let arg_exprs: Vec<Expr> = args
                        .iter()
                        .copied()
                        .map(|v| value_expr(v, st_name, vmctx))
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
                        .map(|v| value_expr(v, st_name, vmctx))
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
            let binds = vec![(
                format!("_unused_{}", inst.as_u32()),
                Expr::app(
                    builtin_throw(),
                    vec![Expr::string(format!(
                        "wasm2nix: unimplemented inst opcode {op}"
                    ))],
                ),
            )];
            (Some(Step { binds, force: None }), None)
        }
    }
}

fn lower_terminator(func: &ir::Function, inst: Inst, st_name: &str, vmctx: ir::Value) -> Expr {
    let data = &func.dfg.insts[inst];
    let op = data.opcode();

    match (op, data) {
        (Opcode::Return, InstructionData::MultiAry { args, .. }) => {
            let rvs = args
                .as_slice(&func.dfg.value_lists)
                .iter()
                .copied()
                .map(|v| value_expr(v, st_name, vmctx))
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
            let target = destination.block(&func.dfg.value_lists);
            let args = destination
                .args_slice(&func.dfg.value_lists)
                .iter()
                .copied()
                .map(|v| value_expr(v, st_name, vmctx))
                .collect::<Vec<_>>();
            let mut call_args = args;
            call_args.push(Expr::ident(st_name));
            Expr::app(Expr::ident(block_name(target)), call_args)
        }

        (Opcode::Brif, InstructionData::Brif { blocks, arg, .. }) => {
            let cond = value_expr(*arg, st_name, vmctx);

            let then_call = {
                let bc = blocks[0];
                let target = bc.block(&func.dfg.value_lists);
                let mut a = bc
                    .args_slice(&func.dfg.value_lists)
                    .iter()
                    .copied()
                    .map(|v| value_expr(v, st_name, vmctx))
                    .collect::<Vec<_>>();
                a.push(Expr::ident(st_name));
                Expr::app(Expr::ident(block_name(target)), a)
            };

            let else_call = {
                let bc = blocks[1];
                let target = bc.block(&func.dfg.value_lists);
                let mut a = bc
                    .args_slice(&func.dfg.value_lists)
                    .iter()
                    .copied()
                    .map(|v| value_expr(v, st_name, vmctx))
                    .collect::<Vec<_>>();
                a.push(Expr::ident(st_name));
                Expr::app(Expr::ident(block_name(target)), a)
            };

            Expr::if_then_else(cond, then_call, else_call)
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
            let c = value_expr(*arg, st_name, vmctx);
            Expr::if_then_else(
                // trapz: if c == 0 then trap
                Expr::bin(BinOp::Eq, c.clone(), Expr::int(0)),
                Expr::app(
                    builtin_throw(),
                    vec![Expr::string(format!("wasm2nix: trapz {code}"))],
                ),
                Expr::attrset(
                    false,
                    vec![
                        (AttrKey::ident("st"), Expr::ident(st_name)),
                        (AttrKey::ident("results"), Expr::list(vec![])),
                    ],
                ),
            )
        }

        (Opcode::Trapnz, InstructionData::CondTrap { arg, code, .. }) => {
            let c = value_expr(*arg, st_name, vmctx);
            Expr::if_then_else(
                // trapnz: if c != 0 then trap
                Expr::bin(BinOp::Neq, c.clone(), Expr::int(0)),
                Expr::app(
                    builtin_throw(),
                    vec![Expr::string(format!("wasm2nix: trapnz {code}"))],
                ),
                Expr::attrset(
                    false,
                    vec![
                        (AttrKey::ident("st"), Expr::ident(st_name)),
                        (AttrKey::ident("results"), Expr::list(vec![])),
                    ],
                ),
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

#[derive(Debug, Clone, Copy)]
enum CallKind {
    Pure,
    StatefulRecord,
    StatefulState,
}

fn resolve_callee(func: &ir::Function, name: ir::ExternalName) -> (Expr, CallKind) {
    match name {
        ir::ExternalName::User(user_ref) => {
            let user = &func.params.user_named_funcs()[user_ref];
            match user.namespace {
                FUNC_NAMESPACE => {
                    // wasm function: f{index}
                    (
                        Expr::ident(format!("f{}", user.index)),
                        CallKind::StatefulRecord,
                    )
                }
                INTRINSIC_NAMESPACE => match user.index {
                    INTR_MEM_GROW => (rt_fn(&["mem", "grow"]), CallKind::StatefulRecord),
                    INTR_MEM_SIZE => (rt_fn(&["mem", "size"]), CallKind::Pure),

                    INTR_GLOBAL_GET_I32 => (rt_fn(&["global", "get_i32"]), CallKind::Pure),
                    INTR_GLOBAL_GET_I64 => (rt_fn(&["global", "get_i64"]), CallKind::Pure),
                    INTR_GLOBAL_GET_F32 => (rt_fn(&["global", "get_f32"]), CallKind::Pure),
                    INTR_GLOBAL_GET_F64 => (rt_fn(&["global", "get_f64"]), CallKind::Pure),

                    INTR_GLOBAL_SET_I32 => (rt_fn(&["global", "set_i32"]), CallKind::StatefulState),
                    INTR_GLOBAL_SET_I64 => (rt_fn(&["global", "set_i64"]), CallKind::StatefulState),
                    INTR_GLOBAL_SET_F32 => (rt_fn(&["global", "set_f32"]), CallKind::StatefulState),
                    INTR_GLOBAL_SET_F64 => (rt_fn(&["global", "set_f64"]), CallKind::StatefulState),

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
        Opcode::Sload8 => "load_s8",
        Opcode::Uload16 => "load_u16",
        Opcode::Sload16 => "load_s16",
        Opcode::Uload32 => "load_u32",
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

fn value_name(v: ir::Value) -> String {
    format!("v{}", v.as_u32())
}

fn block_name(b: Block) -> String {
    format!("b{}", b.as_u32())
}

fn value_expr(v: ir::Value, st_name: &str, vmctx: ir::Value) -> Expr {
    if v == vmctx {
        Expr::ident(st_name)
    } else {
        Expr::ident(value_name(v))
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
