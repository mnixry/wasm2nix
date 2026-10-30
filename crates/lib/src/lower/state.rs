//! The initial instance state `st0`: memory, globals, tables, and the
//! passive segments.

use std::collections::BTreeMap;

use cranelift_entity::EntityRef;
use snafu::OptionExt;
use wasm2nix_runtime::MEMORY_CHUNK_WORDS;
use wasm2nix_runtime::MEMORY_GROUP_CHUNKS;
use wasm2nix_runtime::RuntimeFunction;

use crate::lower::const_expr::lower_const_expr;
use crate::lower::error::MissingGlobalInitializerSnafu;
use crate::lower::error::Result;
use crate::lower::imports::lower_attr_or;
use crate::lower::imports::lower_imported_extern;
use crate::lower::names::global_bind_name;
use crate::lower::table_entry::lower_table_entry;
use crate::nix::AttrBind;
use crate::nix::AttrKey;
use crate::nix::Expr;
use crate::nix::LetBind;
use crate::nix::nix;
use crate::translator::NixModule;
use crate::translator::WASM32_MAX_PAGES;

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

    // Validation bounds 32-bit memories by `WASM32_MAX_PAGES`.
    let pages = |count: u64| Expr::int(i64::try_from(count).expect("page count fits in i64"));
    let declared_min = pages(memory.entity.minimum);
    let declared_max = pages(memory.entity.maximum.unwrap_or(WASM32_MAX_PAGES));
    let overlay_pages = lower_initial_memory_pages(module);

    if let Some((module_name, field_name)) = module.info.imported_memories.get(memory_index.index())
    {
        let imported = lower_imported_extern(module_name, field_name);
        nix!(let
            importedMem = #{imported};
        in {
            pageCount = #{lower_attr_or(nix!(importedMem), "pageCount", declared_min)};
            maxPageCount = #{lower_attr_or(nix!(importedMem), "maxPageCount", declared_max)};
            pages = #{RuntimeFunction::MemOverlayPages.call([
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

        // Bytes past the address space are out of bounds; instantiation traps.
        for (addr, byte) in (init.offset..=u64::MAX).zip(init.data.iter().copied()) {
            let word = words.entry(addr / 4).or_insert(0);
            let mut bytes = word.to_le_bytes();
            if let Some(slot) = bytes.get_mut((addr % 4) as usize) {
                *slot = byte;
            }
            *word = u32::from_le_bytes(bytes);
        }
    }

    let mut groups: BTreeMap<u64, BTreeMap<u64, Vec<(u64, Expr)>>> = BTreeMap::new();
    for (idx, word) in words {
        let chunk = idx / MEMORY_CHUNK_WORDS;
        groups
            .entry(chunk / MEMORY_GROUP_CHUNKS)
            .or_default()
            .entry(chunk)
            .or_default()
            .push((idx, Expr::int(i64::from(word))));
    }
    keyed_attrset(groups.into_iter().map(|(group, chunks)| {
        let chunks = chunks
            .into_iter()
            .map(|(chunk, words)| (chunk, keyed_attrset(words)));
        (group, keyed_attrset(chunks))
    }))
}

/// An attrset keyed by the decimal strings of `entries`' indices.
fn keyed_attrset(entries: impl IntoIterator<Item = (u64, Expr)>) -> Expr {
    Expr::attrset(
        entries
            .into_iter()
            .map(|(key, value)| AttrBind::new(AttrKey::string(key.to_string()), value)),
    )
}

fn lower_data_segments(module: &NixModule) -> Expr {
    keyed_attrset(module.info.data_segments.iter().map(|(idx, data)| {
        let bytes = data.iter().map(|byte| Expr::int(i64::from(*byte)));
        (u64::from(idx.as_u32()), Expr::list(bytes.collect()))
    }))
}

fn lower_initial_globals(module: &NixModule) -> Result<Expr> {
    let info = &module.info;
    let mut binds = Vec::new();
    let mut globals = Vec::new();

    for (idx, global) in &info.globals {
        let value = if let Some((module_name, field_name)) = info.imported_globals.get(idx.index())
        {
            lower_imported_extern(module_name, field_name)
        } else {
            let init = info
                .global_inits
                .get(&idx)
                .context(MissingGlobalInitializerSnafu { index: idx })?;
            lower_const_expr(module, init, global.entity.wasm_ty)?
        };

        let name = global_bind_name(idx);
        globals.push((u64::from(idx.as_u32()), Expr::ident(name.as_str())));
        binds.push(LetBind::new(name, value));
    }

    Ok(Expr::let_in(binds, keyed_attrset(globals)))
}

fn lower_initial_tables(module: &NixModule) -> Expr {
    keyed_attrset(module.info.tables.iter().map(|(table_index, table)| {
        let mut elems = BTreeMap::new();
        for init in &module.info.table_initializers {
            if init.table_index != table_index || init.base.is_some() {
                continue;
            }
            for (slot, func_index) in (u64::from(init.offset)..).zip(init.elements.iter().copied())
            {
                elems.insert(slot, lower_table_entry(module, func_index));
            }
        }
        let elems = keyed_attrset(elems);
        let size = i64::from(table.entity.minimum);
        let max_size = i64::from(table.entity.maximum.unwrap_or(u32::MAX));

        let table = if let Some((module_name, field_name)) =
            module.info.imported_tables.get(table_index.index())
        {
            let imported = lower_imported_extern(module_name, field_name);
            nix!(let
                importedTable = #{imported};
            in {
                size = #{lower_attr_or(nix!(importedTable), "size", Expr::int(size))};
                maxSize = #{lower_attr_or(nix!(importedTable), "maxSize", Expr::int(max_size))};
                elems = merge(#{lower_attr_or(nix!(importedTable), "elems", nix!({}))}, #{elems});
            })
        } else {
            nix!({
                size = #{size};
                maxSize = #{max_size};
                elems = #{elems};
            })
        };
        (u64::from(table_index.as_u32()), table)
    }))
}

fn lower_element_segments(module: &NixModule) -> Expr {
    keyed_attrset(module.info.element_segments.iter().map(|(idx, elements)| {
        let entries = elements
            .iter()
            .map(|func_index| lower_table_entry(module, *func_index));
        (u64::from(idx.as_u32()), Expr::list(entries.collect()))
    }))
}
