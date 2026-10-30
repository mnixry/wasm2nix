//! Propagates imported tables, memories and globals between module states.
//! Each new state is bound to a batch variable, so states never nest. Every
//! update reads the current state, so updates of one module chain.

use std::collections::HashMap;

use cranelift_entity::EntityRef;
use snafu::OptionExt;
use wasm2nix::nix::Expr;
use wasm2nix::nix::nix;
use wasm2nix::translator::NixModule;
use wasm2nix_runtime::RuntimeFunction;

use crate::error::MissingTableExportSnafu;
use crate::error::Result;
use crate::error::UnknownModuleSnafu;
use crate::eval::nix_u64;
use crate::script::HarnessModule;
use crate::script::ModuleState;
use crate::script::Session;

/// The registered module an import refers to.
struct ImportTarget {
    /// The import's index among the importing module's imports of its kind.
    import_index: u32,
    import_module: String,
    field: String,
    key: String,
    /// The index of the imported export in the target, if it has one.
    export_index: Option<u32>,
}

impl Session {
    pub fn set_state(&mut self, key: &str, state: Expr) -> Result<()> {
        let state = self.batch.bind(state);
        self.modules
            .get_mut(key)
            .context(UnknownModuleSnafu { key })?
            .state = ModuleState::Pending(state);
        Ok(())
    }

    fn state(&self, key: &str) -> Result<Expr> {
        Ok(self
            .modules
            .get(key)
            .context(UnknownModuleSnafu { key })?
            .state
            .expr())
    }

    /// The registered modules the imports of `importer` refer to; `imports`
    /// picks the imports of one kind and `exports` the target's exports of
    /// that kind.
    fn import_targets(
        &self, importer: &str, imports: fn(&HarnessModule) -> &[(String, String)],
        exports: fn(&HarnessModule) -> &HashMap<String, u32>,
    ) -> Result<Vec<ImportTarget>> {
        let importer = self
            .modules
            .get(importer)
            .context(UnknownModuleSnafu { key: importer })?;
        Ok((0..)
            .zip(imports(importer))
            .filter_map(|(import_index, (import_module, field))| {
                let (key, module) = self
                    .modules
                    .get_key_value(self.world().resolve(import_module))?;
                Some(ImportTarget {
                    import_index,
                    import_module: import_module.clone(),
                    field: field.clone(),
                    key: key.clone(),
                    export_index: exports(module).get(field).copied(),
                })
            })
            .collect())
    }

    /// Propagates the state changes of a call into module `key`.
    pub fn sync_after_call(&mut self, key: &str) -> Result<()> {
        self.sync_global_imports_to_targets(key)?;
        self.sync_memory_imports_to_targets(key)?;
        self.sync_global_imports_from_targets()
    }

    /// Refreshes every imported global from its exporting module.
    pub fn sync_global_imports_from_targets(&mut self) -> Result<()> {
        let keys = self.modules.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            let targets =
                self.import_targets(&key, |m| &m.imported_globals, |m| &m.global_exports)?;
            let mut state = None;
            for target in targets {
                let Some(export_index) = target.export_index else {
                    continue;
                };
                let dst = match state.take() {
                    Some(state) => state,
                    None => self.state(&key)?,
                };
                state = Some(nix!(harness.syncGlobalState({
                    dstSt = #{dst};
                    dstGlobalIndex = #{target.import_index};
                    srcSt = #{self.state(&target.key)?};
                    srcGlobalIndex = #{export_index};
                })));
            }
            if let Some(state) = state {
                self.set_state(&key, state)?;
            }
        }
        Ok(())
    }

    /// Writes the globals module `source_key` imports back to their exporters.
    fn sync_global_imports_to_targets(&mut self, source_key: &str) -> Result<()> {
        let source = self.state(source_key)?;
        let targets =
            self.import_targets(source_key, |m| &m.imported_globals, |m| &m.global_exports)?;
        for target in targets {
            let Some(export_index) = target.export_index else {
                continue;
            };
            let state = nix!(harness.syncGlobalState({
                dstSt = #{self.state(&target.key)?};
                dstGlobalIndex = #{export_index};
                srcSt = #{source.clone()};
                srcGlobalIndex = #{target.import_index};
            }));
            self.set_state(&target.key, state)?;
        }
        Ok(())
    }

    /// Writes the memories module `source_key` imports back to their
    /// exporters.
    pub fn sync_memory_imports_to_targets(&mut self, source_key: &str) -> Result<()> {
        let source = self.state(source_key)?;
        let targets =
            self.import_targets(source_key, |m| &m.imported_memories, |m| &m.memory_exports)?;
        for target in targets {
            if target.export_index.is_none() {
                continue;
            }
            let state = nix!(harness.syncMemoryState({
                dstSt = #{self.state(&target.key)?};
                srcSt = #{source.clone()};
            }));
            self.set_state(&target.key, state)?;
        }
        Ok(())
    }

    /// Copies the tables a freshly instantiated module imports back to their
    /// exporters.
    pub fn merge_imported_table_initializers(&mut self, key: &str) -> Result<()> {
        let source = self.state(key)?;
        let targets = self.import_targets(key, |m| &m.imported_tables, |m| &m.table_exports)?;
        for target in targets {
            let export_index = target.export_index.context(MissingTableExportSnafu {
                module_name: target.import_module,
                field_name: target.field,
            })?;
            let state = nix!(harness.mergeTableState({
                dstSt = #{self.state(&target.key)?};
                targetTableIndex = #{export_index};
                srcSt = #{source.clone()};
                sourceTableIndex = #{target.import_index};
            }));
            self.set_state(&target.key, state)?;
        }
        Ok(())
    }

    /// Applies the in-bounds active element segments of a trapping module to
    /// the tables it imports, up to the first out-of-bounds segment.
    pub fn apply_trapping_table_side_effects(
        &mut self, module: &NixModule, source_state: &Expr,
    ) -> Result<()> {
        for init in &module.info.table_initializers {
            if init.base.is_some() {
                continue;
            }
            let Some((import_module, field)) =
                module.info.imported_tables.get(init.table_index.index())
            else {
                continue;
            };
            let len = init.elements.len() as u64;
            #[expect(clippy::indexing_slicing, reason = "validated table index")]
            let min = u64::from(module.info.tables[init.table_index].entity.minimum);
            if u64::from(init.offset).saturating_add(len) > min {
                break;
            }
            let key = self.world().resolve(import_module).to_string();
            let Some(&target_index) = self
                .modules
                .get(&key)
                .and_then(|target| target.table_exports.get(field))
            else {
                continue;
            };
            let state = nix!(harness.mergeTableEntries({
                dstSt = #{self.state(&key)?};
                targetTableIndex = #{target_index};
                srcSt = #{source_state.clone()};
                sourceTableIndex = #{init.table_index.as_u32()};
                offset = #{init.offset};
                len = #{nix_u64(len)?};
            }));
            self.set_state(&key, state)?;
        }
        Ok(())
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
            #[expect(clippy::indexing_slicing, reason = "validated memory index")]
            let min_pages = module.info.memories[init.memory_index].entity.minimum;
            if init.offset.saturating_add(len) > min_pages.saturating_mul(65536) {
                break;
            }
            let key = self.world().resolve(import_module).to_string();
            if !self
                .modules
                .get(&key)
                .is_some_and(|target| target.memory_exports.contains_key(field))
            {
                continue;
            }
            let offset = nix_u64(init.offset)?;
            let bytes = RuntimeFunction::MemLoadBytes.call([
                offset.clone(),
                nix_u64(len)?,
                source_state.clone(),
            ]);
            let state = RuntimeFunction::MemStoreBytes.call([bytes, offset, self.state(&key)?]);
            self.set_state(&key, state)?;
        }
        Ok(())
    }
}
