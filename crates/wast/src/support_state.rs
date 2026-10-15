use std::collections::HashMap;

use cranelift_entity::EntityRef;
use serde_json::Value as JsonValue;
use snafu::prelude::*;
use wasm2nix::{
    nix::{Expr, nix},
    translator::NixModule,
};
use wasm2nix_runtime::RuntimeFunction;
use wast::{
    WastArg, WastRet,
    core::{NanPattern, WastArgCore, WastRetCore},
};

use crate::{
    error::{
        self, ActualResultsNotArraySnafu, EncodeJsonSnafu, EncodeWatSnafu, Error,
        ExpectedIntegerResultSnafu, ExpectedNumericResultSnafu, LowerNixSnafu,
        MissingTableExportSnafu, NanPatternRequiresPredicateSnafu, Result, SourceMissingStateSnafu,
        TargetMissingStateSnafu, TranslateWasmSnafu, UnknownModuleSnafu,
        UnsupportedAssertReturnResultSnafu, UnsupportedInvokeArgumentSnafu,
    },
    support_core::{
        HarnessContext, HarnessModule, active_initializer_traps, func_exports, global_export_types,
        global_exports, memory_export_limits, memory_exports, table_export_limits, table_exports,
        validate_imports_for_linking, write_generated_module,
    },
    support_eval::{eval_initial_state, eval_instantiate, nix_u64, state_expr},
};

pub fn apply_trapping_module_side_effects(
    ctx: &HarnessContext, directive_index: usize, wat: &mut wast::Wat<'_>,
    modules: &mut HashMap<String, HarnessModule>, aliases: &HashMap<String, String>,
) -> Result<()> {
    let wasm = error::directive(directive_index, wat.encode().context(EncodeWatSnafu))?;
    let translated = error::directive(
        directive_index,
        NixModule::translate(&wasm).context(TranslateWasmSnafu),
    )?;
    error::directive(
        directive_index,
        validate_imports_for_linking(ctx, &translated, modules, aliases),
    )?;
    let initializer_traps = error::directive(
        directive_index,
        active_initializer_traps(ctx, &translated, modules, aliases),
    )?;
    let nix = error::directive(
        directive_index,
        wasm2nix::lowerings::lower_module(&translated).context(LowerNixSnafu),
    )?
    .render();
    let path = write_generated_module(ctx, &nix)?;
    let transient_key = format!("__trap{directive_index}");
    let source_state = error::directive(
        directive_index,
        eval_initial_state(ctx, &path, &transient_key, modules, aliases),
    )?;
    if !initializer_traps && eval_instantiate(ctx, &path, &transient_key, modules, aliases).is_ok()
    {
        return Err(Error::Directive {
            directive_index,
            source: Box::new(Error::TrapModuleInstantiated),
        });
    }
    modules.insert(
        transient_key,
        HarnessModule {
            path,
            state: Some(source_state.clone()),
            imported_tables: translated.info.imported_tables.clone(),
            imported_globals: translated.info.imported_globals.clone(),
            imported_memories: translated.info.imported_memories.clone(),
            func_exports: func_exports(&translated),
            table_exports: table_exports(&translated),
            global_exports: global_exports(&translated),
            memory_exports: memory_exports(&translated),
            table_export_limits: table_export_limits(&translated),
            global_export_types: global_export_types(&translated),
            memory_export_limits: memory_export_limits(&translated),
        },
    );
    apply_trapping_table_side_effects(&translated, &source_state, modules, aliases);
    apply_trapping_memory_side_effects(&translated, &source_state, modules, aliases);
    Ok(())
}

fn apply_trapping_table_side_effects(
    module: &NixModule, source_state: &str, modules: &mut HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) {
    for init in &module.info.table_initializers {
        if init.base.is_some() {
            continue;
        }
        let Some((import_module, import_field)) =
            module.info.imported_tables.get(init.table_index.index())
        else {
            continue;
        };
        let table_size = module.info.tables[init.table_index].entity.minimum;
        let end = init.offset.saturating_add(init.elements.len() as u32);
        if end > table_size {
            break;
        }

        let target_key = aliases
            .get(import_module)
            .cloned()
            .unwrap_or_else(|| import_module.clone());
        let Some(target_module) = modules.get(&target_key) else {
            continue;
        };
        let Some(target_table_index) = target_module.table_exports.get(import_field).copied()
        else {
            continue;
        };
        let Some(target_state) = target_module.state.clone() else {
            continue;
        };
        let merged = merge_table_entries_expr(
            &target_state,
            target_table_index,
            source_state,
            init.table_index.as_u32(),
            init.offset,
            init.elements.len() as u32,
        );
        if let Some(module) = modules.get_mut(&target_key) {
            module.state = Some(merged);
        }
    }
}

fn merge_table_entries_expr(
    target_state: &str, target_table_index: u32, source_state: &str, source_table_index: u32,
    offset: u32, len: u32,
) -> String {
    nix!(harness.mergeTableEntries({
        dstSt = #{state_expr(target_state)};
        targetTableIndex = #{i64::from(target_table_index)};
        srcSt = #{state_expr(source_state)};
        sourceTableIndex = #{i64::from(source_table_index)};
        offset = #{i64::from(offset)};
        len = #{i64::from(len)};
    }))
    .render()
}

fn apply_trapping_memory_side_effects(
    module: &NixModule, source_state: &str, modules: &mut HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) {
    for init in &module.info.data_initializers {
        if init.base.is_some() {
            continue;
        }
        let Some((import_module, import_field)) =
            module.info.imported_memories.get(init.memory_index.index())
        else {
            continue;
        };
        let memory_min_pages = module.info.memories[init.memory_index].entity.minimum;
        let end = init.offset.saturating_add(init.data.len() as u64);
        if end > memory_min_pages.saturating_mul(65536) {
            break;
        }

        let target_key = aliases
            .get(import_module)
            .cloned()
            .unwrap_or_else(|| import_module.clone());
        let Some(target_module) = modules.get(&target_key) else {
            continue;
        };
        if !target_module.memory_exports.contains_key(import_field) {
            continue;
        }
        let Some(target_state) = target_module.state.clone() else {
            continue;
        };
        let merged =
            merge_memory_words_expr(&target_state, source_state, init.offset, init.data.len());
        if let Some(module) = modules.get_mut(&target_key) {
            module.state = Some(merged);
        }
    }
}

fn merge_memory_words_expr(
    target_state: &str, source_state: &str, offset: u64, len: usize,
) -> String {
    let offset = nix_u64(offset).unwrap_or_else(|_| Expr::raw(offset.to_string()));
    let len = nix_u64(len as u64).unwrap_or_else(|_| Expr::raw(len.to_string()));
    let bytes =
        RuntimeFunction::MemLoadBytes.call(vec![offset.clone(), len, state_expr(source_state)]);
    RuntimeFunction::MemStoreBytes
        .call(vec![bytes, offset, state_expr(target_state)])
        .render()
}

pub fn sync_global_imports_from_targets(
    modules: &mut HashMap<String, HarnessModule>, aliases: &HashMap<String, String>,
) {
    let mut keys = modules.keys().cloned().collect::<Vec<_>>();
    keys.sort();

    for source_key in keys {
        let Some(source_module) = modules.get(&source_key) else {
            continue;
        };
        let Some(mut source_state) = source_module.state.clone() else {
            continue;
        };
        let imports = source_module.imported_globals.clone();

        for (source_global_index, (import_module, import_field)) in imports.iter().enumerate() {
            let target_key = aliases
                .get(import_module)
                .cloned()
                .unwrap_or_else(|| import_module.clone());
            let Some(target_module) = modules.get(&target_key) else {
                continue;
            };
            let Some(target_global_index) = target_module.global_exports.get(import_field).copied()
            else {
                continue;
            };
            let Some(target_state) = target_module.state.clone() else {
                continue;
            };
            source_state = sync_global_state_expr(
                &source_state,
                source_global_index as u32,
                &target_state,
                target_global_index,
            );
        }

        if let Some(module) = modules.get_mut(&source_key) {
            module.state = Some(source_state);
        }
    }
}

pub fn sync_global_imports_to_targets(
    source_key: &str, modules: &mut HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) {
    let Some(source_module) = modules.get(source_key) else {
        return;
    };
    let Some(source_state) = source_module.state.clone() else {
        return;
    };
    let imports = source_module.imported_globals.clone();

    for (source_global_index, (import_module, import_field)) in imports.iter().enumerate() {
        let target_key = aliases
            .get(import_module)
            .cloned()
            .unwrap_or_else(|| import_module.clone());
        let Some(target_module) = modules.get(&target_key) else {
            continue;
        };
        let Some(target_global_index) = target_module.global_exports.get(import_field).copied()
        else {
            continue;
        };
        let Some(target_state) = target_module.state.clone() else {
            continue;
        };
        let synced = sync_global_state_expr(
            &target_state,
            target_global_index,
            &source_state,
            source_global_index as u32,
        );
        if let Some(module) = modules.get_mut(&target_key) {
            module.state = Some(synced);
        }
    }
}

fn sync_global_state_expr(
    dst_state: &str, dst_global_index: u32, src_state: &str, src_global_index: u32,
) -> String {
    nix!(harness.syncGlobalState({
        dstSt = #{state_expr(dst_state)};
        dstGlobalIndex = #{i64::from(dst_global_index)};
        srcSt = #{state_expr(src_state)};
        srcGlobalIndex = #{i64::from(src_global_index)};
    }))
    .render()
}

pub fn sync_memory_imports_to_targets(
    source_key: &str, modules: &mut HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) {
    let Some(source_module) = modules.get(source_key) else {
        return;
    };
    let Some(source_state) = source_module.state.clone() else {
        return;
    };
    let imports = source_module.imported_memories.clone();

    for (source_memory_index, (import_module, import_field)) in imports.iter().enumerate() {
        let target_key = aliases
            .get(import_module)
            .cloned()
            .unwrap_or_else(|| import_module.clone());
        let Some(target_module) = modules.get(&target_key) else {
            continue;
        };
        if !target_module.memory_exports.contains_key(import_field) {
            continue;
        }
        let Some(target_state) = target_module.state.clone() else {
            continue;
        };
        let synced =
            sync_memory_state_expr(&target_state, &source_state, source_memory_index as u32);
        if let Some(module) = modules.get_mut(&target_key) {
            module.state = Some(synced);
        }
    }
}

fn sync_memory_state_expr(dst_state: &str, src_state: &str, src_memory_index: u32) -> String {
    nix!(harness.syncMemoryState({
        dstSt = #{state_expr(dst_state)};
        srcSt = #{state_expr(src_state)};
        srcMemoryIndex = #{i64::from(src_memory_index)};
    }))
    .render()
}

pub fn merge_imported_table_initializers(
    module_key: &str, modules: &mut HashMap<String, HarnessModule>,
    aliases: &HashMap<String, String>,
) -> Result<()> {
    let imported_tables = modules
        .get(module_key)
        .context(UnknownModuleSnafu {
            key: module_key.to_string(),
        })?
        .imported_tables
        .clone();

    for (source_table_index, (import_module, import_field)) in imported_tables.iter().enumerate() {
        let target_key = aliases
            .get(import_module)
            .cloned()
            .unwrap_or_else(|| import_module.clone());
        if !modules.contains_key(&target_key) {
            continue;
        }

        let target_table_index = modules[&target_key]
            .table_exports
            .get(import_field)
            .copied()
            .context(MissingTableExportSnafu {
                module_name: import_module.clone(),
                field_name: import_field.clone(),
            })?;
        let target_state = modules[&target_key]
            .state
            .clone()
            .context(TargetMissingStateSnafu {
                key: target_key.clone(),
            })?;
        let source_state = modules[module_key]
            .state
            .clone()
            .context(SourceMissingStateSnafu {
                key: module_key.to_string(),
            })?;

        let merged = merge_table_state_expr(
            &target_state,
            target_table_index,
            &source_state,
            source_table_index as u32,
        );
        modules
            .get_mut(&target_key)
            .context(UnknownModuleSnafu {
                key: target_key.clone(),
            })?
            .state = Some(merged);
    }

    Ok(())
}

fn merge_table_state_expr(
    target_state: &str, target_table_index: u32, source_state: &str, source_table_index: u32,
) -> String {
    nix!(harness.mergeTableState({
        dstSt = #{state_expr(target_state)};
        targetTableIndex = #{i64::from(target_table_index)};
        srcSt = #{state_expr(source_state)};
        sourceTableIndex = #{i64::from(source_table_index)};
    }))
    .render()
}

pub fn wast_arg_to_nix(arg: &WastArg<'_>) -> Result<Expr> {
    match arg {
        WastArg::Core(WastArgCore::I32(value)) => Ok(Expr::int(i64::from(value.cast_unsigned()))),
        WastArg::Core(WastArgCore::I64(value)) => Ok(Expr::int(*value)),
        WastArg::Core(WastArgCore::F32(value)) => Ok(Expr::int(i64::from(value.bits))),
        WastArg::Core(WastArgCore::F64(value)) => Ok(Expr::int(value.bits.cast_signed())),
        WastArg::Core(WastArgCore::RefExtern(value)) => {
            Ok(nix!({ extern = #{i64::from(*value)}; }))
        }
        WastArg::Core(WastArgCore::RefNull(_)) => Ok(Expr::int(0)),
        _ => UnsupportedInvokeArgumentSnafu.fail(),
    }
}

pub fn json_to_nix_expr(value: &JsonValue) -> Result<String> {
    let json = serde_json::to_string(value).context(EncodeJsonSnafu)?;
    Ok(nix!(harness.fromJSON(#{json})).render())
}

pub fn expected_results_json(results: &[WastRet<'_>]) -> Result<String> {
    let values = results
        .iter()
        .map(expected_result_label)
        .collect::<Result<Vec<_>>>()?;
    Ok(format!("[{}]", values.join(",")))
}

fn expected_result_label(ret: &WastRet<'_>) -> Result<String> {
    match ret {
        WastRet::Core(WastRetCore::I32(value)) => Ok(value.cast_unsigned().to_string()),
        WastRet::Core(WastRetCore::I64(value)) => Ok(value.to_string()),
        WastRet::Core(WastRetCore::F32(NanPattern::Value(value))) => Ok(value.bits.to_string()),
        WastRet::Core(WastRetCore::F64(NanPattern::Value(value))) => {
            Ok(value.bits.cast_signed().to_string())
        }
        WastRet::Core(
            WastRetCore::F32(NanPattern::CanonicalNan) | WastRetCore::F64(NanPattern::CanonicalNan),
        ) => Ok("nan:canonical".into()),
        WastRet::Core(
            WastRetCore::F32(NanPattern::ArithmeticNan)
            | WastRetCore::F64(NanPattern::ArithmeticNan),
        ) => Ok("nan:arithmetic".into()),
        WastRet::Core(WastRetCore::RefExtern(Some(value))) => Ok(format!("{{\"extern\":{value}}}")),
        WastRet::Core(WastRetCore::RefNull(_)) => Ok("0".to_string()),
        _ => UnsupportedAssertReturnResultSnafu.fail(),
    }
}

fn expected_result_value(ret: &WastRet<'_>) -> Result<JsonValue> {
    match ret {
        WastRet::Core(WastRetCore::I32(value)) => Ok(JsonValue::from(value.cast_unsigned())),
        WastRet::Core(WastRetCore::I64(value)) => Ok(JsonValue::from(*value)),
        WastRet::Core(WastRetCore::F32(NanPattern::Value(value))) => {
            Ok(JsonValue::from(value.bits))
        }
        WastRet::Core(WastRetCore::F64(NanPattern::Value(value))) => {
            Ok(JsonValue::from(value.bits.cast_signed()))
        }
        WastRet::Core(WastRetCore::RefExtern(Some(value))) => {
            Ok(serde_json::json!({ "extern": value }))
        }
        WastRet::Core(WastRetCore::RefNull(_)) => Ok(JsonValue::from(0)),
        WastRet::Core(
            WastRetCore::F32(NanPattern::CanonicalNan | NanPattern::ArithmeticNan)
            | WastRetCore::F64(NanPattern::CanonicalNan | NanPattern::ArithmeticNan),
        ) => NanPatternRequiresPredicateSnafu.fail(),
        _ => UnsupportedAssertReturnResultSnafu.fail(),
    }
}

pub fn results_match(expected: &[WastRet<'_>], actual: &JsonValue) -> Result<bool> {
    let JsonValue::Array(actual_items) = actual else {
        return ActualResultsNotArraySnafu.fail();
    };
    if actual_items.len() != expected.len() {
        return Ok(false);
    }
    for (expected, actual) in expected.iter().zip(actual_items.iter()) {
        if !result_matches(expected, actual)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn result_matches(expected: &WastRet<'_>, actual: &JsonValue) -> Result<bool> {
    match expected {
        WastRet::Core(WastRetCore::F32(NanPattern::CanonicalNan)) => {
            let bits = json_number_to_u64(actual)? as u32;
            Ok(f32_canonical_nan(bits))
        }
        WastRet::Core(WastRetCore::F32(NanPattern::ArithmeticNan)) => {
            let bits = json_number_to_u64(actual)? as u32;
            Ok(f32_arithmetic_nan(bits))
        }
        WastRet::Core(WastRetCore::F64(NanPattern::CanonicalNan)) => {
            let bits = json_number_to_u64(actual)?;
            Ok(f64_canonical_nan(bits))
        }
        WastRet::Core(WastRetCore::F64(NanPattern::ArithmeticNan)) => {
            let bits = json_number_to_u64(actual)?;
            Ok(f64_arithmetic_nan(bits))
        }
        _ => Ok(*actual == expected_result_value(expected)?),
    }
}

pub fn json_number_to_u64(value: &JsonValue) -> Result<u64> {
    let JsonValue::Number(number) = value else {
        return ExpectedNumericResultSnafu.fail();
    };
    number.as_u64().map_or_else(
        || {
            number
                .as_i64()
                .map(i64::cast_unsigned)
                .context(ExpectedIntegerResultSnafu)
        },
        Ok,
    )
}

const fn f32_canonical_nan(bits: u32) -> bool {
    (bits & 0x7F80_0000) == 0x7F80_0000 && (bits & 0x007F_FFFF) == 0x0040_0000
}

const fn f32_arithmetic_nan(bits: u32) -> bool {
    (bits & 0x7F80_0000) == 0x7F80_0000 && (bits & 0x0040_0000) != 0
}

const fn f64_canonical_nan(bits: u64) -> bool {
    (bits & 0x7FF0_0000_0000_0000) == 0x7FF0_0000_0000_0000
        && (bits & 0x000F_FFFF_FFFF_FFFF) == 0x0008_0000_0000_0000
}

const fn f64_arithmetic_nan(bits: u64) -> bool {
    (bits & 0x7FF0_0000_0000_0000) == 0x7FF0_0000_0000_0000 && (bits & 0x0008_0000_0000_0000) != 0
}
