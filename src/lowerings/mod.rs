use std::collections::BTreeMap;

mod error;
mod inst;
mod types;

use cranelift_codegen::ir;
use cranelift_entity::EntityRef;
use cranelift_wasm::{ConstOp, WasmValType};
use error::MalformedConstExprSnafu;
pub use error::{LowerError, Result};
use snafu::prelude::*;

use crate::{
    nix::{AttrKey, BinOp, Expr},
    translator::NixModule,
};

pub fn lower_module(module: &NixModule) -> Result<Expr> {
    // Top-level:
    // rt: imports:
    // let moduleId = ...; funcs = rec { ... }; funcRegistry = ...; st0 = ...; st =
    // ...; exports = ...; in exports // { instantiate = args: { st = st;
    // exports = exports; }; __wasm2nix = { funcs = funcs; }; }
    let funcs_expr = lower_funcs_rec_attrset(module)?;
    let exports_expr = lower_exports_attrset(module, ExportKeyMode::Public)?;
    let exports_by_name_expr = lower_exports_attrset(module, ExportKeyMode::Encoded)?;
    let st0_expr = lower_initial_state(module)?;
    let instantiate_expr = Expr::lambda(
        "args",
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
                            (AttrKey::ident("st0"), Expr::ident("st0")),
                            (
                                AttrKey::ident("exportsByName"),
                                Expr::ident("exportsByName"),
                            ),
                        ],
                    ),
                ),
            ],
        ),
    );

    Ok(Expr::lambda(
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
                    ("exportsByName".into(), exports_by_name_expr),
                ],
                body,
            ),
        ),
    ))
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

fn lower_funcs_rec_attrset(module: &NixModule) -> Result<Expr> {
    let mut binds: Vec<(AttrKey, Expr)> = Vec::new();

    for (idx, (module_name, field_name)) in module.info.imported_funcs.iter().enumerate() {
        let func_index = cranelift_wasm::FuncIndex::from_u32(idx as u32);
        let sig_index = module.info.functions[func_index].entity;
        let imported =
            lower_imported_func(module_name, field_name, &module.info.signatures[sig_index]);
        binds.push((AttrKey::ident(format!("f{idx}")), imported));
    }

    for (defined_idx, func) in module.info.function_bodies.iter() {
        let func_index = module.num_func_imports() as u32 + defined_idx.as_u32();
        let name = format!("f{func_index}");
        let lowered = inst::lower_function(module, func_index, func)?;
        binds.push((AttrKey::ident(name), lowered));
    }

    Ok(Expr::attrset(true, binds))
}

fn lower_imported_func(module_name: &str, field_name: &str, sig: &ir::Signature) -> Expr {
    let module = Expr::select(
        Expr::ident("imports"),
        AttrKey::string(module_name.to_string()),
    );
    let imported = Expr::select(module.clone(), AttrKey::string(field_name.to_string()));
    let mut params = (0..sig.params.len())
        .map(|idx| format!("arg{idx}"))
        .collect::<Vec<_>>();
    params.push("st".to_string());

    let mut call_args = params
        .iter()
        .take(sig.params.len())
        .cloned()
        .map(Expr::ident)
        .collect::<Vec<_>>();
    call_args.push(lower_imported_extern_state(module));
    let call = Expr::app(imported, call_args);
    let body = Expr::let_in(
        vec![("call".into(), call)],
        Expr::attrset(
            false,
            vec![
                (
                    AttrKey::ident("results"),
                    Expr::select(Expr::ident("call"), AttrKey::ident("results")),
                ),
                (AttrKey::ident("st"), Expr::ident("st")),
            ],
        ),
    );
    lambda_chain(&params, body)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportKeyMode {
    Public,
    Encoded,
}

fn lower_exports_attrset(module: &NixModule, mode: ExportKeyMode) -> Result<Expr> {
    let mut binds: Vec<(AttrKey, Expr)> = Vec::new();

    for (func_index, func_decl) in module.info.functions.iter() {
        if func_decl.export_names.is_empty() {
            continue;
        }
        let f_name = format!("f{}", func_index.as_u32());
        let f_expr = Expr::select(Expr::ident("funcs"), AttrKey::ident(f_name));
        for export_name in &func_decl.export_names {
            let Some(key) = export_attr_key(export_name, mode) else {
                continue;
            };
            // Use string keys for safety (names can contain characters invalid in
            // identifiers).
            binds.push((AttrKey::string(key), f_expr.clone()));
        }
    }

    for (memory_index, memory_decl) in module.info.memories.iter() {
        for export_name in &memory_decl.export_names {
            let Some(key) = export_attr_key(export_name, mode) else {
                continue;
            };
            binds.push((
                AttrKey::string(key),
                lower_memory_export(memory_index.as_u32()),
            ));
        }
    }

    for (table_index, table_decl) in module.info.tables.iter() {
        for export_name in &table_decl.export_names {
            let Some(key) = export_attr_key(export_name, mode) else {
                continue;
            };
            binds.push((
                AttrKey::string(key),
                lower_table_export(table_index.as_u32()),
            ));
        }
    }

    for (global_index, global_decl) in module.info.globals.iter() {
        for export_name in &global_decl.export_names {
            let Some(key) = export_attr_key(export_name, mode) else {
                continue;
            };
            binds.push((
                AttrKey::string(key),
                lower_global_export(global_index.as_u32(), global_decl.entity.wasm_ty)?,
            ));
        }
    }

    Ok(Expr::attrset(false, binds))
}

fn export_attr_key(export_name: &str, mode: ExportKeyMode) -> Option<String> {
    match mode {
        ExportKeyMode::Public => nix_source_string_safe(export_name).then(|| export_name.into()),
        ExportKeyMode::Encoded => Some(encoded_export_key(export_name)),
    }
}

fn nix_source_string_safe(s: &str) -> bool {
    !s.contains('\0')
}

fn encoded_export_key(name: &str) -> String {
    let mut encoded = String::from("__wasm2nix_export_");
    for byte in name.as_bytes() {
        push_hex_byte(&mut encoded, *byte);
    }
    encoded
}

fn push_hex_byte(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(HEX[(byte >> 4) as usize] as char);
    out.push(HEX[(byte & 0x0F) as usize] as char);
}

fn lower_initial_state(module: &NixModule) -> Result<Expr> {
    Ok(Expr::attrset(
        false,
        vec![
            (AttrKey::ident("mem"), lower_initial_memory(module)),
            (AttrKey::ident("globals"), lower_initial_globals(module)?),
            (AttrKey::ident("tables"), lower_initial_tables(module)),
            (AttrKey::ident("data"), lower_data_segments(module)),
            (
                AttrKey::ident("elemSegments"),
                lower_element_segments(module),
            ),
        ],
    ))
}

fn lower_initial_memory(module: &NixModule) -> Expr {
    let Some((memory_index, memory)) = module.info.memories.iter().next() else {
        return Expr::attrset(
            false,
            vec![
                (AttrKey::ident("pageCount"), Expr::int(0)),
                (AttrKey::ident("maxPageCount"), Expr::int(0)),
                (AttrKey::ident("pages"), Expr::attrset(false, vec![])),
            ],
        );
    };

    let declared_min = Expr::int(memory.entity.minimum as i64);
    let declared_max_pages = memory.entity.maximum.unwrap_or(65536);
    let declared_max = Expr::int(declared_max_pages as i64);
    let overlay_pages = lower_initial_memory_pages(module);

    if let Some((module_name, field_name)) = module.info.imported_memories.get(memory_index.index())
    {
        let imported = lower_imported_memory(module_name, field_name);
        Expr::let_in(
            vec![("importedMem".into(), imported)],
            Expr::attrset(
                false,
                vec![
                    (
                        AttrKey::ident("pageCount"),
                        lower_attr_or(Expr::ident("importedMem"), "pageCount", declared_min),
                    ),
                    (
                        AttrKey::ident("maxPageCount"),
                        lower_attr_or(Expr::ident("importedMem"), "maxPageCount", declared_max),
                    ),
                    (
                        AttrKey::ident("pages"),
                        Expr::bin(
                            BinOp::Update,
                            lower_attr_or(
                                Expr::ident("importedMem"),
                                "pages",
                                Expr::attrset(false, vec![]),
                            ),
                            overlay_pages,
                        ),
                    ),
                ],
            ),
        )
    } else {
        Expr::attrset(
            false,
            vec![
                (AttrKey::ident("pageCount"), declared_min),
                (AttrKey::ident("maxPageCount"), declared_max),
                (AttrKey::ident("pages"), overlay_pages),
            ],
        )
    }
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
            let mask = !(0xFF_u32 << shift);
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

fn lower_imported_memory(module_name: &str, field_name: &str) -> Expr {
    let module = Expr::select(
        Expr::ident("imports"),
        AttrKey::string(module_name.to_string()),
    );
    let imported = Expr::select(module.clone(), AttrKey::string(field_name.to_string()));
    Expr::if_then_else(
        Expr::bin(
            BinOp::And,
            Expr::app(builtin_is_attrs(), vec![imported.clone()]),
            Expr::app(
                builtin_has_attr(),
                vec![Expr::string("get"), imported.clone()],
            ),
        ),
        Expr::app(
            Expr::select(imported.clone(), AttrKey::ident("get")),
            vec![lower_imported_extern_state(module)],
        ),
        imported,
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

fn lower_initial_globals(module: &NixModule) -> Result<Expr> {
    let mut binds = Vec::new();
    let mut global_attrs = Vec::new();

    for (idx, global) in module.info.globals.iter() {
        let bind_name = global_bind_name(idx);
        let value = if let Some((module_name, field_name)) =
            module.info.imported_globals.get(idx.index())
        {
            lower_imported_global(module_name, field_name)
        } else if let Some(init) = module
            .info
            .global_inits
            .get(idx)
            .and_then(|expr| expr.as_ref())
        {
            lower_const_expr(module, init, global.entity.wasm_ty)?
        } else {
            Expr::app(
                builtin_throw(),
                vec![Expr::string("wasm2nix: global has no initializer")],
            )
        };

        binds.push((bind_name.clone(), value));
        global_attrs.push((
            AttrKey::string(idx.as_u32().to_string()),
            Expr::ident(bind_name),
        ));
    }

    Ok(Expr::let_in(binds, Expr::attrset(false, global_attrs)))
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

        let table_expr = if let Some((module_name, field_name)) =
            module.info.imported_tables.get(table_index.index())
        {
            let imported = lower_imported_table(module_name, field_name);
            Expr::let_in(
                vec![("importedTable".into(), imported)],
                Expr::attrset(
                    false,
                    vec![
                        (
                            AttrKey::ident("size"),
                            lower_attr_or(
                                Expr::ident("importedTable"),
                                "size",
                                Expr::int(i64::from(table_decl.entity.minimum)),
                            ),
                        ),
                        (
                            AttrKey::ident("maxSize"),
                            lower_attr_or(
                                Expr::ident("importedTable"),
                                "maxSize",
                                Expr::int(i64::from(table_decl.entity.maximum.unwrap_or(u32::MAX))),
                            ),
                        ),
                        (
                            AttrKey::ident("elems"),
                            Expr::bin(
                                BinOp::Update,
                                lower_attr_or(
                                    Expr::ident("importedTable"),
                                    "elems",
                                    Expr::attrset(false, vec![]),
                                ),
                                elems_expr,
                            ),
                        ),
                    ],
                ),
            )
        } else {
            Expr::attrset(
                false,
                vec![
                    (
                        AttrKey::ident("size"),
                        Expr::int(i64::from(table_decl.entity.minimum)),
                    ),
                    (
                        AttrKey::ident("maxSize"),
                        Expr::int(i64::from(table_decl.entity.maximum.unwrap_or(u32::MAX))),
                    ),
                    (AttrKey::ident("elems"), elems_expr),
                ],
            )
        };

        table_binds.push((
            AttrKey::string(table_index.as_u32().to_string()),
            table_expr,
        ));
    }

    Expr::attrset(false, table_binds)
}

fn lower_imported_table(module_name: &str, field_name: &str) -> Expr {
    let module = Expr::select(
        Expr::ident("imports"),
        AttrKey::string(module_name.to_string()),
    );
    let imported = Expr::select(module.clone(), AttrKey::string(field_name.to_string()));
    Expr::if_then_else(
        Expr::bin(
            BinOp::And,
            Expr::app(builtin_is_attrs(), vec![imported.clone()]),
            Expr::app(
                builtin_has_attr(),
                vec![Expr::string("get"), imported.clone()],
            ),
        ),
        Expr::app(
            Expr::select(imported.clone(), AttrKey::ident("get")),
            vec![lower_imported_extern_state(module)],
        ),
        imported,
    )
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
    Expr::attrset(
        false,
        vec![
            (AttrKey::ident("module"), Expr::ident("moduleId")),
            (
                AttrKey::ident("func"),
                Expr::int(i64::from(func_index.as_u32())),
            ),
            (
                AttrKey::ident("type"),
                lower_signature_id(module, function.entity),
            ),
        ],
    )
}

fn lower_signature_id(module: &NixModule, sig_index: cranelift_wasm::TypeIndex) -> Expr {
    Expr::string(signature_id(&module.info.signatures[sig_index]))
}

fn signature_id(sig: &ir::Signature) -> String {
    let params = sig
        .params
        .iter()
        .map(|param| param.value_type.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let returns = sig
        .returns
        .iter()
        .map(|param| param.value_type.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!("({params})->({returns})")
}

fn lower_imported_global(module_name: &str, field_name: &str) -> Expr {
    let module = Expr::select(
        Expr::ident("imports"),
        AttrKey::string(module_name.to_string()),
    );
    let imported = Expr::select(module.clone(), AttrKey::string(field_name.to_string()));
    Expr::if_then_else(
        Expr::bin(
            BinOp::And,
            Expr::app(builtin_is_attrs(), vec![imported.clone()]),
            Expr::app(
                builtin_has_attr(),
                vec![Expr::string("get"), imported.clone()],
            ),
        ),
        Expr::app(
            Expr::select(imported.clone(), AttrKey::ident("get")),
            vec![lower_imported_extern_state(module)],
        ),
        imported,
    )
}

fn lower_imported_extern_state(module: Expr) -> Expr {
    Expr::if_then_else(
        Expr::bin(
            BinOp::And,
            Expr::app(builtin_is_attrs(), vec![module.clone()]),
            Expr::app(
                builtin_has_attr(),
                vec![Expr::string("__wasm2nixState"), module.clone()],
            ),
        ),
        Expr::select(module.clone(), AttrKey::string("__wasm2nixState")),
        Expr::if_then_else(
            Expr::bin(
                BinOp::And,
                Expr::app(builtin_is_attrs(), vec![module.clone()]),
                Expr::app(
                    builtin_has_attr(),
                    vec![Expr::string("instantiate"), module.clone()],
                ),
            ),
            Expr::select(
                Expr::app(
                    Expr::select(module, AttrKey::ident("instantiate")),
                    vec![Expr::attrset(false, vec![])],
                ),
                AttrKey::ident("st"),
            ),
            Expr::attrset(false, vec![]),
        ),
    )
}

fn lower_attr_or(expr: Expr, attr: &str, default: Expr) -> Expr {
    Expr::if_then_else(
        Expr::bin(
            BinOp::And,
            Expr::app(builtin_is_attrs(), vec![expr.clone()]),
            Expr::app(
                builtin_has_attr(),
                vec![Expr::string(attr.to_string()), expr.clone()],
            ),
        ),
        Expr::select(expr, AttrKey::ident(attr.to_string())),
        default,
    )
}

fn global_bind_name(idx: cranelift_wasm::GlobalIndex) -> String {
    format!("g{}", idx.as_u32())
}

fn lower_const_expr(
    module: &NixModule, expr: &cranelift_wasm::ConstExpr, ty: WasmValType,
) -> Result<Expr> {
    let mut stack = Vec::new();

    for op in expr.ops().iter().copied() {
        match op {
            ConstOp::I32Const(value) => stack.push(Expr::int(i64::from(value as u32))),
            ConstOp::I64Const(value) => stack.push(Expr::int(value)),
            ConstOp::F32Const(bits) => stack.push(Expr::int(i64::from(bits))),
            ConstOp::F64Const(bits) => stack.push(Expr::int(bits as i64)),
            ConstOp::GlobalGet(idx) => stack.push(Expr::ident(global_bind_name(idx))),
            ConstOp::RefNull if matches!(ty, WasmValType::Ref(_)) => stack.push(Expr::int(0)),
            ConstOp::RefFunc(func_index) if matches!(ty, WasmValType::Ref(_)) => {
                stack.push(lower_table_entry(module, func_index));
            }
            ConstOp::I32Add | ConstOp::I32Sub | ConstOp::I32Mul => {
                let rhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: format!("{op:?} missing right operand"),
                })?;
                let lhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: format!("{op:?} missing left operand"),
                })?;
                let op_name = match op {
                    ConstOp::I32Add => "add",
                    ConstOp::I32Sub => "sub",
                    ConstOp::I32Mul => "mul",
                    _ => {
                        return MalformedConstExprSnafu {
                            reason: format!("unexpected operator {op:?}"),
                        }
                        .fail();
                    }
                };
                stack.push(Expr::app(rt_fn(&["i32", op_name]), vec![lhs, rhs]));
            }
            ConstOp::I64Add | ConstOp::I64Sub | ConstOp::I64Mul => {
                let rhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: format!("{op:?} missing right operand"),
                })?;
                let lhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: format!("{op:?} missing left operand"),
                })?;
                let op_name = match op {
                    ConstOp::I64Add => "add",
                    ConstOp::I64Sub => "sub",
                    ConstOp::I64Mul => "mul",
                    _ => {
                        return MalformedConstExprSnafu {
                            reason: format!("unexpected operator {op:?}"),
                        }
                        .fail();
                    }
                };
                stack.push(Expr::app(rt_fn(&["i64", op_name]), vec![lhs, rhs]));
            }
            _ => {
                return MalformedConstExprSnafu {
                    reason: format!("unsupported operator {op:?}"),
                }
                .fail();
            }
        }
    }

    ensure!(
        stack.len() == 1,
        MalformedConstExprSnafu {
            reason: format!("expected one result, found {}", stack.len()),
        }
    );
    stack.pop().context(MalformedConstExprSnafu {
        reason: "empty stack".to_string(),
    })
}

fn lower_memory_export(memory_index: u32) -> Expr {
    debug_assert_eq!(memory_index, 0);
    Expr::bin(
        BinOp::Update,
        Expr::select(Expr::ident("rt"), AttrKey::ident("mem")),
        Expr::attrset(
            false,
            vec![(
                AttrKey::ident("get"),
                Expr::lambda("st", Expr::select(Expr::ident("st"), AttrKey::ident("mem"))),
            )],
        ),
    )
}

fn lower_table_export(table_index: u32) -> Expr {
    Expr::bin(
        BinOp::Update,
        Expr::select(Expr::ident("rt"), AttrKey::ident("table")),
        Expr::attrset(
            false,
            vec![(
                AttrKey::ident("get"),
                Expr::lambda(
                    "st",
                    Expr::select(
                        Expr::select(Expr::ident("st"), AttrKey::ident("tables")),
                        AttrKey::string(table_index.to_string()),
                    ),
                ),
            )],
        ),
    )
}

fn lower_global_export(global_index: u32, ty: WasmValType) -> Result<Expr> {
    let get = Expr::lambda(
        "st",
        Expr::app(
            types::global_get_expr(ty)?,
            vec![Expr::int(i64::from(global_index)), Expr::ident("st")],
        ),
    );
    let set = Expr::lambda(
        "value",
        Expr::lambda(
            "st",
            Expr::app(
                types::global_set_expr(ty)?,
                vec![
                    Expr::int(i64::from(global_index)),
                    Expr::ident("value"),
                    Expr::ident("st"),
                ],
            ),
        ),
    );

    Ok(Expr::attrset(
        false,
        vec![(AttrKey::ident("get"), get), (AttrKey::ident("set"), set)],
    ))
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

fn builtin_foldl_strict() -> Expr {
    Expr::select(Expr::ident("builtins"), AttrKey::string("foldl'"))
}

fn builtin_generic_closure() -> Expr {
    Expr::select(Expr::ident("builtins"), AttrKey::ident("genericClosure"))
}

fn builtin_get_attr() -> Expr {
    Expr::select(Expr::ident("builtins"), AttrKey::ident("getAttr"))
}

fn builtin_has_attr() -> Expr {
    Expr::select(Expr::ident("builtins"), AttrKey::ident("hasAttr"))
}

fn builtin_is_attrs() -> Expr {
    Expr::select(Expr::ident("builtins"), AttrKey::ident("isAttrs"))
}

fn builtin_length() -> Expr {
    Expr::select(Expr::ident("builtins"), AttrKey::ident("length"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translator::NixModule;

    fn lower_wasm(wasm: &[u8]) -> String {
        let module = match NixModule::translate(wasm) {
            Ok(module) => module,
            Err(err) => panic!("wasm should translate: {err}"),
        };
        match lower_module(&module) {
            Ok(expr) => expr.render(),
            Err(err) => panic!("wasm should lower: {err}"),
        }
    }

    fn lower_wat(wat: &str) -> String {
        let buf = match wast::parser::ParseBuffer::new(wat) {
            Ok(buf) => buf,
            Err(err) => panic!("wat should parse: {err}"),
        };
        let mut wat = match wast::parser::parse::<wast::Wat<'_>>(&buf) {
            Ok(wat) => wat,
            Err(err) => panic!("wat should parse: {err}"),
        };
        let wasm = match wat.encode() {
            Ok(wasm) => wasm,
            Err(err) => panic!("wat should encode: {err}"),
        };
        lower_wasm(&wasm)
    }

    #[test]
    fn generated_module_keeps_compat_exports_and_instantiate() {
        let wasm = [
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x01, 0x07, 0x01, 0x60, 0x02, 0x7F,
            0x7F, 0x01, 0x7F, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64,
            0x00, 0x00, 0x0A, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6A, 0x0B,
        ];
        let nix = lower_wasm(&wasm);

        assert!(nix.contains("\"add\" = funcs.f0;"));
        assert!(nix.contains("instantiate = args:"));
        assert!(nix.contains("st = st;"));
        assert!(nix.contains("exports = exports;"));
        assert!(nix.contains("__wasm2nix ="));
    }

    #[test]
    fn generated_module_embeds_active_data_segments() {
        let wasm = [
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01,
            0x7F, 0x03, 0x02, 0x01, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07, 0x11, 0x02, 0x06,
            0x6D, 0x65, 0x6D, 0x6F, 0x72, 0x79, 0x02, 0x00, 0x04, 0x6C, 0x6F, 0x61, 0x64, 0x00,
            0x00, 0x0A, 0x09, 0x01, 0x07, 0x00, 0x41, 0x04, 0x28, 0x02, 0x00, 0x0B, 0x0B, 0x0A,
            0x01, 0x00, 0x41, 0x04, 0x0B, 0x04, 0x01, 0x02, 0x03, 0x04,
        ];
        let nix = lower_wasm(&wasm);

        assert!(nix.contains("pageCount = 1;"));
        assert!(nix.contains("\"1\" = 67305985;"));
        assert!(nix.contains("\"memory\" = rt.mem // {"));
    }

    #[test]
    fn generated_module_runs_start_before_exposing_instance_state() {
        let wasm = [
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
            0x03, 0x02, 0x01, 0x00, 0x06, 0x06, 0x01, 0x7F, 0x01, 0x41, 0x00, 0x0B, 0x07, 0x05,
            0x01, 0x01, 0x67, 0x03, 0x00, 0x08, 0x01, 0x00, 0x0A, 0x08, 0x01, 0x06, 0x00, 0x41,
            0x2A, 0x24, 0x00, 0x0B,
        ];
        let nix = lower_wasm(&wasm);

        assert!(nix.contains("g0 = 0;"));
        assert!(nix.contains("\"0\" = g0;"));
        assert!(nix.contains("st = (funcs.f0 st0).st;"));
        assert!(nix.contains("rt.global.set_i32"));
        assert!(nix.contains("\"g\" = {"));
    }

    #[test]
    fn generated_module_initializes_imported_global_get_and_ref_null_globals() {
        let nix = lower_wat(
            r#"
            (module
              (global (import "spectest" "global_i32") i32)
              (global $copy i32 (global.get 0))
              (global $r externref (ref.null extern))
              (func (export "copy") (result i32) (global.get $copy))
              (func (export "ref") (result externref) (global.get $r)))
            "#,
        );

        assert!(nix.contains("g0 = if builtins.isAttrs imports.\"spectest\".\"global_i32\""));
        assert!(nix.contains("g1 = g0;"));
        assert!(nix.contains("g2 = 0;"));
        assert!(nix.contains("\"0\" = g0;"));
        assert!(nix.contains("\"1\" = g1;"));
        assert!(nix.contains("\"2\" = g2;"));
    }

    #[test]
    fn generated_module_lowers_call_indirect_through_table_runtime() {
        let wasm = [
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7F,
            0x01, 0x7F, 0x03, 0x03, 0x02, 0x00, 0x00, 0x04, 0x04, 0x01, 0x70, 0x00, 0x01, 0x07,
            0x08, 0x01, 0x04, 0x63, 0x61, 0x6C, 0x6C, 0x00, 0x01, 0x09, 0x07, 0x01, 0x00, 0x41,
            0x00, 0x0B, 0x01, 0x00, 0x0A, 0x13, 0x02, 0x07, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6A,
            0x0B, 0x09, 0x00, 0x20, 0x00, 0x41, 0x00, 0x11, 0x00, 0x00, 0x0B,
        ];
        let nix = lower_wasm(&wasm);

        assert!(nix.contains("rt.table.call 0 \"(i32)->(i32)\" moduleId funcs funcRegistry"));
        assert!(nix.contains("tables = {"));
        assert!(nix.contains("module = moduleId;"));
        assert!(nix.contains("func = 0;"));
        assert!(nix.contains("type = \"(i32)->(i32)\";"));
    }
}
