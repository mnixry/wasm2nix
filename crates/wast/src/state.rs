//! Propagates imported tables, memories and globals between module states.
//! Each new state is bound to a batch variable, so states never nest.

use cranelift_entity::EntityRef;
use snafu::prelude::*;
use wasm2nix::{
    nix::{Expr, nix},
    translator::NixModule,
};
use wasm2nix_runtime::RuntimeFunction;

use crate::{
    error::{MissingTableExportSnafu, Result, UnknownModuleSnafu},
    eval::nix_u64,
    script::{HarnessModule, Session, resolve_alias},
};

impl Session {
    pub fn set_state(&mut self, key: &str, state: Expr) {
        let state = self.batch.bind(state);
        if let Some(module) = self.modules.get_mut(key) {
            module.state = state;
            module.pending = true;
        }
    }

    fn target(&self, import_module: &str) -> Option<(String, &HarnessModule)> {
        let key = resolve_alias(&self.aliases, import_module);
        let module = self.modules.get(&key)?;
        Some((key, module))
    }

    /// Propagates the state changes of a call into module `key`.
    pub fn sync_after_call(&mut self, key: &str) {
        self.sync_global_imports_to_targets(key);
        self.sync_memory_imports_to_targets(key);
        self.sync_global_imports_from_targets();
    }

    /// Refreshes every imported global from its exporting module.
    pub fn sync_global_imports_from_targets(&mut self) {
        let keys = self.modules.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            let module = &self.modules[&key];
            let mut state = module.state.clone();
            let mut synced = false;
            for (index, (import_module, field)) in module.imported_globals.iter().enumerate() {
                let Some((_, target)) = self.target(import_module) else {
                    continue;
                };
                let Some(&target_index) = target.global_exports.get(field) else {
                    continue;
                };
                state = nix!(harness.syncGlobalState({
                    dstSt = #{state};
                    dstGlobalIndex = #{i64::from(index as u32)};
                    srcSt = #{target.state.clone()};
                    srcGlobalIndex = #{i64::from(target_index)};
                }));
                synced = true;
            }
            if synced {
                self.set_state(&key, state);
            }
        }
    }

    /// Writes the globals module `source_key` imports back to their exporters.
    fn sync_global_imports_to_targets(&mut self, source_key: &str) {
        let Some(source) = self.modules.get(source_key) else {
            return;
        };
        let source_state = source.state.clone();
        for (index, (import_module, field)) in source.imported_globals.clone().iter().enumerate() {
            let Some((target_key, target)) = self.target(import_module) else {
                continue;
            };
            let Some(&target_index) = target.global_exports.get(field) else {
                continue;
            };
            let state = nix!(harness.syncGlobalState({
                dstSt = #{target.state.clone()};
                dstGlobalIndex = #{i64::from(target_index)};
                srcSt = #{source_state.clone()};
                srcGlobalIndex = #{i64::from(index as u32)};
            }));
            self.set_state(&target_key, state);
        }
    }

    /// Writes the memories module `source_key` imports back to their
    /// exporters.
    pub fn sync_memory_imports_to_targets(&mut self, source_key: &str) {
        let Some(source) = self.modules.get(source_key) else {
            return;
        };
        let source_state = source.state.clone();
        for (import_module, field) in source.imported_memories.clone() {
            let Some((target_key, target)) = self.target(&import_module) else {
                continue;
            };
            if !target.memory_exports.contains_key(&field) {
                continue;
            }
            let state = nix!(harness.syncMemoryState({
                dstSt = #{target.state.clone()};
                srcSt = #{source_state.clone()};
            }));
            self.set_state(&target_key, state);
        }
    }

    /// Copies the tables a freshly instantiated module imports back to their
    /// exporters.
    pub fn merge_imported_table_initializers(&mut self, key: &str) -> Result<()> {
        let source = self.modules.get(key).context(UnknownModuleSnafu { key })?;
        let source_state = source.state.clone();
        for (index, (import_module, field)) in source.imported_tables.clone().iter().enumerate() {
            let Some((target_key, target)) = self.target(import_module) else {
                continue;
            };
            let target_index =
                *target
                    .table_exports
                    .get(field)
                    .context(MissingTableExportSnafu {
                        module_name: import_module,
                        field_name: field,
                    })?;
            let state = nix!(harness.mergeTableState({
                dstSt = #{target.state.clone()};
                targetTableIndex = #{i64::from(target_index)};
                srcSt = #{source_state.clone()};
                sourceTableIndex = #{i64::from(index as u32)};
            }));
            self.set_state(&target_key, state);
        }
        Ok(())
    }

    /// Applies the in-bounds active element segments of a trapping module to
    /// the tables it imports, up to the first out-of-bounds segment.
    pub fn apply_trapping_table_side_effects(&mut self, module: &NixModule, source_state: &Expr) {
        for init in &module.info.table_initializers {
            if init.base.is_some() {
                continue;
            }
            let Some((import_module, field)) =
                module.info.imported_tables.get(init.table_index.index())
            else {
                continue;
            };
            let len = init.elements.len() as u32;
            if init.offset.saturating_add(len) > module.info.tables[init.table_index].entity.minimum
            {
                break;
            }
            let Some((target_key, target)) = self.target(import_module) else {
                continue;
            };
            let Some(&target_index) = target.table_exports.get(field) else {
                continue;
            };
            let state = nix!(harness.mergeTableEntries({
                dstSt = #{target.state.clone()};
                targetTableIndex = #{i64::from(target_index)};
                srcSt = #{source_state.clone()};
                sourceTableIndex = #{i64::from(init.table_index.as_u32())};
                offset = #{i64::from(init.offset)};
                len = #{i64::from(len)};
            }));
            self.set_state(&target_key, state);
        }
    }

    /// Applies the in-bounds active data segments of a trapping module to the
    /// memories it imports, up to the first out-of-bounds segment.
    pub fn apply_trapping_memory_side_effects(
        &mut self, module: &NixModule, source_state: &Expr,
    ) -> Result<()> {
        for init in &module.info.data_initializers {
            if init.base.is_some() {
                continue;
            }
            let Some((import_module, field)) =
                module.info.imported_memories.get(init.memory_index.index())
            else {
                continue;
            };
            let len = init.data.len() as u64;
            let min_pages = module.info.memories[init.memory_index].entity.minimum;
            if init.offset.saturating_add(len) > min_pages.saturating_mul(65536) {
                break;
            }
            let Some((target_key, target)) = self.target(import_module) else {
                continue;
            };
            if !target.memory_exports.contains_key(field) {
                continue;
            }
            let offset = nix_u64(init.offset)?;
            let bytes = RuntimeFunction::MemLoadBytes.call(vec![
                offset.clone(),
                nix_u64(len)?,
                source_state.clone(),
            ]);
            let state =
                RuntimeFunction::MemStoreBytes.call(vec![bytes, offset, target.state.clone()]);
            self.set_state(&target_key, state);
        }
        Ok(())
    }
}
