use std::collections::BTreeMap;

mod error;
mod inst;
mod ops;
mod types;

use cranelift_codegen::ir;
use cranelift_entity::EntityRef;
use cranelift_wasm::{ConstOp, WasmValType};
use error::MalformedConstExprSnafu;
pub use error::{LowerError, Result};
use snafu::prelude::*;
use wasm2nix_runtime::{RuntimeFunction, RuntimeNamespace};

use crate::{
    nix::{AttrKey, Expr, nix},
    translator::NixModule,
};

/// Lowers the translated module metadata and functions to a Nix expression.
///
/// # Errors
///
/// Returns an error when a Wasm construct is malformed or not yet supported by
/// the lowering layer.
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
    let instantiate_expr = nix!(args: { st = st; exports = exports; });
    let body = nix!(merge(exports, {
        instantiate = #{instantiate_expr};
        __wasm2nix = {
            funcs = funcs;
            moduleId = moduleId;
            st0 = st0;
            exportsByName = exportsByName;
        };
    }));

    Ok(nix!(rt: imports: let
        moduleId = #{lower_module_id()};
        funcs = #{funcs_expr};
        funcRegistry = #{lower_func_registry()};
        st0 = #{st0_expr};
        st = #{lower_start_state(module)};
        exports = #{exports_expr};
        exportsByName = #{exports_by_name_expr};
    in #{body}))
}

fn lower_module_id() -> Expr {
    nix!(if builtins.hasAttr("__moduleId", imports) then imports."__moduleId" else "__self")
}

fn lower_func_registry() -> Expr {
    nix!(merge(
        (if builtins.hasAttr("__funcRegistry", imports) then imports."__funcRegistry" else {}),
        builtins.listToAttrs([{ name = moduleId; value = funcs; }])
    ))
}

fn lower_start_state(module: &NixModule) -> Expr {
    module.info.start_func.map_or_else(
        || nix!(st0),
        |start| {
            let start_func =
                Expr::select(nix!(funcs), AttrKey::ident(format!("f{}", start.as_u32())));
            nix!((#{start_func}(st0)).st)
        },
    )
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

    for (defined_idx, func) in &module.info.function_bodies {
        let func_index = module.num_func_imports() as u32 + defined_idx.as_u32();
        let name = format!("f{func_index}");
        let lowered = inst::lower_function(module, func_index, func)?;
        binds.push((AttrKey::ident(name), lowered));
    }

    Ok(Expr::attrset(true, binds))
}

fn lower_imported_func(module_name: &str, field_name: &str, sig: &ir::Signature) -> Expr {
    let module = nix!(imports.(module_name));
    let imported = nix!(#{module.clone()}.(field_name));
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
    let body = nix!(let call = #{call}; in { results = call.results; st = st; });
    lambda_chain(&params, body)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportKeyMode {
    Public,
    Encoded,
}

fn lower_exports_attrset(module: &NixModule, mode: ExportKeyMode) -> Result<Expr> {
    let mut binds: Vec<(AttrKey, Expr)> = Vec::new();

    for (func_index, func_decl) in &module.info.functions {
        if func_decl.export_names.is_empty() {
            continue;
        }
        let f_name = format!("f{}", func_index.as_u32());
        let f_expr = Expr::select(nix!(funcs), AttrKey::ident(f_name));
        for export_name in &func_decl.export_names {
            let Some(key) = export_attr_key(export_name, mode) else {
                continue;
            };
            // Use string keys for safety (names can contain characters invalid in
            // identifiers).
            binds.push((AttrKey::string(key), f_expr.clone()));
        }
    }

    for (memory_index, memory_decl) in &module.info.memories {
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

    for (table_index, table_decl) in &module.info.tables {
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

    for (global_index, global_decl) in &module.info.globals {
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
    Ok(nix!({
        mem = #{lower_initial_memory(module)};
        globals = #{lower_initial_globals(module)?};
        tables = #{lower_initial_tables(module)};
        data = #{lower_data_segments(module)};
        elemSegments = #{lower_element_segments(module)};
    }))
}

fn lower_initial_memory(module: &NixModule) -> Expr {
    let Some((memory_index, memory)) = module.info.memories.iter().next() else {
        return nix!({
            pageCount = 0;
            maxPageCount = 0;
            pages = {};
        });
    };

    let declared_min =
        Expr::int(i64::try_from(memory.entity.minimum).expect("memory minimum fits in i64"));
    let declared_max_pages = memory.entity.maximum.unwrap_or(65536);
    let declared_max =
        Expr::int(i64::try_from(declared_max_pages).expect("memory maximum fits in i64"));
    let overlay_pages = lower_initial_memory_pages(module);

    if let Some((module_name, field_name)) = module.info.imported_memories.get(memory_index.index())
    {
        let imported = lower_imported_memory(module_name, field_name);
        nix!(let
            importedMem = #{imported};
        in {
            pageCount = #{lower_attr_or(nix!(importedMem), "pageCount", declared_min)};
            maxPageCount = #{lower_attr_or(nix!(importedMem), "maxPageCount", declared_max)};
            pages = merge(#{lower_attr_or(nix!(importedMem), "pages", nix!({}))}, #{overlay_pages});
        })
    } else {
        nix!({
            pageCount = #{declared_min};
            maxPageCount = #{declared_max};
            pages = #{overlay_pages};
        })
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
    let module = nix!(imports.(module_name));
    let imported = nix!(#{module.clone()}.(field_name));
    nix!(if builtins.isAttrs(#{imported.clone()}) && builtins.hasAttr("get", #{imported.clone()})
        then #{imported.clone()}.get(#{lower_imported_extern_state(module)})
        else #{imported})
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

    for (idx, global) in &module.info.globals {
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
            nix!(throw("wasm2nix: global has no initializer"))
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

    for (table_index, table_decl) in &module.info.tables {
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
            nix!(let
                importedTable = #{imported};
            in {
                size = #{lower_attr_or(
                    nix!(importedTable),
                    "size",
                    nix!(#{i64::from(table_decl.entity.minimum)}),
                )};
                maxSize = #{lower_attr_or(
                    nix!(importedTable),
                    "maxSize",
                    nix!(#{i64::from(table_decl.entity.maximum.unwrap_or(u32::MAX))}),
                )};
                elems = merge(#{lower_attr_or(nix!(importedTable), "elems", nix!({}))}, #{elems_expr});
            })
        } else {
            nix!({
                size = #{i64::from(table_decl.entity.minimum)};
                maxSize = #{i64::from(table_decl.entity.maximum.unwrap_or(u32::MAX))};
                elems = #{elems_expr};
            })
        };

        table_binds.push((
            AttrKey::string(table_index.as_u32().to_string()),
            table_expr,
        ));
    }

    Expr::attrset(false, table_binds)
}

fn lower_imported_table(module_name: &str, field_name: &str) -> Expr {
    let module = nix!(imports.(module_name));
    let imported = nix!(#{module.clone()}.(field_name));
    nix!(if builtins.isAttrs(#{imported.clone()}) && builtins.hasAttr("get", #{imported.clone()})
        then #{imported.clone()}.get(#{lower_imported_extern_state(module)})
        else #{imported})
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
        return nix!(null);
    };
    nix!({
        module = moduleId;
        func = #{i64::from(func_index.as_u32())};
        type = #{lower_signature_id(module, function.entity)};
    })
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
    let module = nix!(imports.(module_name));
    let imported = nix!(#{module.clone()}.(field_name));
    nix!(if builtins.isAttrs(#{imported.clone()}) && builtins.hasAttr("get", #{imported.clone()})
        then #{imported.clone()}.get(#{lower_imported_extern_state(module)})
        else #{imported})
}

fn lower_imported_extern_state(module: Expr) -> Expr {
    nix!(if builtins.isAttrs(#{module.clone()}) && builtins.hasAttr("__wasm2nixState", #{module.clone()})
        then #{module.clone()}."__wasm2nixState"
        else if builtins.isAttrs(#{module.clone()}) && builtins.hasAttr("instantiate", #{module.clone()})
            then (#{module}.instantiate({})).st
            else {})
}

fn lower_attr_or(expr: Expr, attr: &str, default: Expr) -> Expr {
    nix!(if builtins.isAttrs(#{expr.clone()}) && builtins.hasAttr(#{attr}, #{expr.clone()})
        then #{expr}.(attr)
        else #{default})
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
            ConstOp::I32Const(value) => stack.push(Expr::int(i64::from(value.cast_unsigned()))),
            ConstOp::I64Const(value) => stack.push(Expr::int(value)),
            ConstOp::F32Const(bits) => stack.push(Expr::int(i64::from(bits))),
            ConstOp::F64Const(bits) => stack.push(Expr::int(bits.cast_signed())),
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
                let function = match op {
                    ConstOp::I32Add => RuntimeFunction::I32Add,
                    ConstOp::I32Sub => RuntimeFunction::I32Sub,
                    ConstOp::I32Mul => RuntimeFunction::I32Mul,
                    _ => {
                        return MalformedConstExprSnafu {
                            reason: format!("unexpected operator {op:?}"),
                        }
                        .fail();
                    }
                };
                stack.push(function.call(vec![lhs, rhs]));
            }
            ConstOp::I64Add | ConstOp::I64Sub | ConstOp::I64Mul => {
                let rhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: format!("{op:?} missing right operand"),
                })?;
                let lhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: format!("{op:?} missing left operand"),
                })?;
                let function = match op {
                    ConstOp::I64Add => RuntimeFunction::I64Add,
                    ConstOp::I64Sub => RuntimeFunction::I64Sub,
                    ConstOp::I64Mul => RuntimeFunction::I64Mul,
                    _ => {
                        return MalformedConstExprSnafu {
                            reason: format!("unexpected operator {op:?}"),
                        }
                        .fail();
                    }
                };
                stack.push(function.call(vec![lhs, rhs]));
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
    nix!(merge(#{RuntimeNamespace::Mem.expr()}, { get = st: st.mem; }))
}

fn lower_table_export(table_index: u32) -> Expr {
    let table_key = table_index.to_string();
    nix!(merge(#{RuntimeNamespace::Table.expr()}, { get = st: st.tables.(table_key); }))
}

fn lower_global_export(global_index: u32, ty: WasmValType) -> Result<Expr> {
    let get = types::global_get_function(ty)?;
    let set = types::global_set_function(ty)?;
    let index = i64::from(global_index);
    let get_call = get.call(vec![Expr::int(index), nix!(st)]);
    let set_call = set.call(vec![Expr::int(index), nix!(value), nix!(st)]);
    Ok(nix!({
        get = st: #{get_call};
        set = value: st: #{set_call};
    }))
}

fn lambda_chain(params: &[String], body: Expr) -> Expr {
    params
        .iter()
        .rev()
        .fold(body, |acc, p| Expr::lambda(p.clone(), acc))
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

    fn assert_contains_all<I, S>(haystack: &str, needles: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>, {
        for needle in needles {
            let needle = needle.as_ref();
            assert!(
                haystack.contains(needle),
                "expected generated Nix to contain {needle}"
            );
        }
    }

    fn runtime_exprs(functions: impl IntoIterator<Item = RuntimeFunction>) -> Vec<String> {
        functions
            .into_iter()
            .map(|function| function.expr().render())
            .collect()
    }

    #[test]
    fn generated_module_inlines_single_use_immediates_into_bindings() {
        let nix = lower_wat(
            r#"
            (module
              (func (export "add_consts") (result i32)
                i32.const 40
                i32.const 2
                i32.add))
            "#,
        );

        let expected = RuntimeFunction::I32Add
            .call(vec![Expr::int(40), Expr::int(2)])
            .render();
        assert!(nix.contains(&expected));
        assert!(
            !nix.contains(" = 40;") && !nix.contains(" = 2;"),
            "single-use immediate should be inlined instead of bound"
        );
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
        assert!(nix.contains("builtins.deepSeq nextKeyed nextKeyed"));
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
        assert!(nix.contains(&format!(
            "\"memory\" = {} // {{",
            RuntimeNamespace::Mem.expr().render()
        )));
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
        assert!(nix.contains(&RuntimeFunction::GlobalSetI32.expr().render()));
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

        assert!(nix.contains("g0 = if"));
        assert!(nix.contains("builtins.isAttrs imports.\"spectest\".\"global_i32\""));
        assert!(nix.contains("g1 = g0;"));
        assert!(nix.contains("g2 = 0;"));
        assert!(nix.contains("\"0\" = g0;"));
        assert!(nix.contains("\"1\" = g1;"));
        assert!(nix.contains("\"2\" = g2;"));
    }

    #[test]
    fn generated_module_lowers_memory_intrinsics_to_runtime_calls() {
        let nix = lower_wat(
            r#"
            (module
              (memory 1)
              (data $d "abc")
              (func (export "m")
                (drop (memory.size))
                (drop (memory.grow (i32.const 0)))
                (memory.copy (i32.const 0) (i32.const 1) (i32.const 1))
                (memory.fill (i32.const 0) (i32.const 0) (i32.const 1))
                (memory.init $d (i32.const 0) (i32.const 0) (i32.const 1))
                (data.drop $d)))
            "#,
        );

        assert_contains_all(
            &nix,
            runtime_exprs([
                RuntimeFunction::MemSize,
                RuntimeFunction::MemGrow,
                RuntimeFunction::MemCopy,
                RuntimeFunction::MemFill,
                RuntimeFunction::MemInit,
                RuntimeFunction::MemDataDrop,
            ]),
        );
    }

    #[test]
    fn generated_module_lowers_table_intrinsics_to_runtime_calls() {
        let nix = lower_wat(
            r#"
            (module
              (table $t 2 funcref)
              (elem $e funcref (ref.null func))
              (func (export "t")
                (drop (table.size $t))
                (drop (table.grow $t (ref.null func) (i32.const 0)))
                (drop (table.get $t (i32.const 0)))
                (table.set $t (i32.const 0) (ref.null func))
                (table.fill $t (i32.const 0) (ref.null func) (i32.const 1))
                (table.copy $t $t (i32.const 0) (i32.const 1) (i32.const 1))
                (table.init $t $e (i32.const 0) (i32.const 0) (i32.const 1))
                (elem.drop $e)))
            "#,
        );

        assert_contains_all(
            &nix,
            runtime_exprs([
                RuntimeFunction::TableSize,
                RuntimeFunction::TableGrow,
                RuntimeFunction::TableGet,
                RuntimeFunction::TableSet,
                RuntimeFunction::TableFill,
                RuntimeFunction::TableCopy,
                RuntimeFunction::TableInit,
                RuntimeFunction::TableElemDrop,
            ]),
        );
    }

    #[test]
    fn generated_module_lowers_typed_global_intrinsics_to_runtime_calls() {
        let nix = lower_wat(
            r#"
            (module
              (global $i32 (mut i32) (i32.const 0))
              (global $i64 (mut i64) (i64.const 0))
              (global $f32 (mut f32) (f32.const 0))
              (global $f64 (mut f64) (f64.const 0))
              (func (export "g")
                (global.set $i32 (global.get $i32))
                (global.set $i64 (global.get $i64))
                (global.set $f32 (global.get $f32))
                (global.set $f64 (global.get $f64))))
            "#,
        );

        assert_contains_all(
            &nix,
            runtime_exprs([
                RuntimeFunction::GlobalGetI32,
                RuntimeFunction::GlobalGetI64,
                RuntimeFunction::GlobalGetF32,
                RuntimeFunction::GlobalGetF64,
                RuntimeFunction::GlobalSetI32,
                RuntimeFunction::GlobalSetI64,
                RuntimeFunction::GlobalSetF32,
                RuntimeFunction::GlobalSetF64,
            ]),
        );
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

        assert!(nix.contains(&format!(
            "{} 0 \"(i32)->(i32)\" moduleId funcs funcRegistry",
            RuntimeFunction::TableCall.expr().render()
        )));
        assert!(nix.contains("tables = {"));
        assert!(nix.contains("module = moduleId;"));
        assert!(nix.contains("func = 0;"));
        assert!(nix.contains("type = \"(i32)->(i32)\";"));
    }
}
