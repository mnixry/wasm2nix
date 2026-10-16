use std::collections::BTreeMap;

use cranelift_codegen::ir::{self};
use cranelift_entity::EntityRef;
use wasm2nix_runtime::{MEMORY_CHUNK_WORDS, MEMORY_GROUP_CHUNKS, RuntimeFunction};

use crate::{
    lower::{
        const_expr::lower_const_expr,
        error::Result,
        imports::{lower_attr_or, lower_imported_extern},
    },
    nix::{AttrKey, Expr, nix},
    translator::NixModule,
};

pub(super) fn lower_initial_state(module: &NixModule) -> Result<Expr> {
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
        let imported = lower_imported_extern(module_name, field_name);
        nix!(let
            importedMem = #{imported};
        in {
            pageCount = #{lower_attr_or(nix!(importedMem), "pageCount", declared_min)};
            maxPageCount = #{lower_attr_or(nix!(importedMem), "maxPageCount", declared_max)};
            pages = #{RuntimeFunction::MemOverlayPages.call(vec![
                lower_attr_or(nix!(importedMem), "pages", nix!({})),
                overlay_pages,
            ])};
        })
    } else {
        nix!({
            pageCount = #{declared_min};
            maxPageCount = #{declared_max};
            pages = #{overlay_pages};
        })
    }
}

/// Lays out active data segments as the runtime's three-level page attrset:
/// groups of [`MEMORY_GROUP_CHUNKS`] chunks of [`MEMORY_CHUNK_WORDS`]
/// little-endian u32 words, each level keyed by its global index.
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

    let mut groups: BTreeMap<u64, BTreeMap<u64, Vec<(AttrKey, Expr)>>> = BTreeMap::new();
    for (idx, word) in words {
        let chunk = idx / MEMORY_CHUNK_WORDS;
        groups
            .entry(chunk / MEMORY_GROUP_CHUNKS)
            .or_default()
            .entry(chunk)
            .or_default()
            .push((AttrKey::string(idx.to_string()), Expr::int(i64::from(word))));
    }
    keyed_attrset(groups.into_iter().map(|(group, chunks)| {
        let chunks = chunks
            .into_iter()
            .map(|(chunk, words)| (chunk, Expr::attrset(false, words)));
        (group, keyed_attrset(chunks))
    }))
}

fn keyed_attrset(entries: impl Iterator<Item = (u64, Expr)>) -> Expr {
    Expr::attrset(
        false,
        entries
            .map(|(key, value)| (AttrKey::string(key.to_string()), value))
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

fn lower_initial_globals(module: &NixModule) -> Result<Expr> {
    let mut binds = Vec::new();
    let mut global_attrs = Vec::new();

    for (idx, global) in &module.info.globals {
        let bind_name = global_bind_name(idx);
        let value = if let Some((module_name, field_name)) =
            module.info.imported_globals.get(idx.index())
        {
            lower_imported_extern(module_name, field_name)
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
            let imported = lower_imported_extern(module_name, field_name);
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

pub(super) fn lower_table_entry(module: &NixModule, func_index: cranelift_wasm::FuncIndex) -> Expr {
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

pub(super) fn signature_id(sig: &ir::Signature) -> String {
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

pub(super) fn global_bind_name(idx: cranelift_wasm::GlobalIndex) -> String {
    format!("g{}", idx.as_u32())
}
