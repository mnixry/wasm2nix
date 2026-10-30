use cranelift_codegen::ir;
use cranelift_codegen::ir::UserFuncName;
use cranelift_codegen::ir::types::F32;
use cranelift_codegen::ir::types::F64;
use cranelift_codegen::ir::types::I32;
use cranelift_codegen::ir::types::I64;
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::isa::TargetFrontendConfig;
use cranelift_entity::EntityRef;
use cranelift_entity::PrimaryMap;
use cranelift_entity::SecondaryMap;
use cranelift_wasm::ConstExpr;
use cranelift_wasm::DataIndex;
use cranelift_wasm::DefinedFuncIndex;
use cranelift_wasm::ElemIndex;
use cranelift_wasm::FuncIndex;
use cranelift_wasm::FuncTranslator;
use cranelift_wasm::Global;
use cranelift_wasm::GlobalIndex;
use cranelift_wasm::Memory;
use cranelift_wasm::MemoryIndex;
use cranelift_wasm::ModuleEnvironment;
use cranelift_wasm::Table;
use cranelift_wasm::TableIndex;
use cranelift_wasm::TypeIndex;
use cranelift_wasm::WasmError;
use cranelift_wasm::WasmFuncType;
use cranelift_wasm::WasmResult;
use cranelift_wasm::WasmValType;
use cranelift_wasm::translate_module;
use snafu::ResultExt;
use target_lexicon::PointerWidth;
use wasm2nix_runtime::intrinsics::UserName;

use crate::error::TranslateError;
use crate::error::TranslateSnafu;
use crate::func_env::NixFuncEnvironment;
use crate::func_env::user_name;

#[derive(Debug, Clone)]
pub struct Exportable<T> {
    pub entity: T,
    pub export_names: Vec<String>,
}

impl<T> Exportable<T> {
    const fn new(entity: T) -> Self {
        Self {
            entity,
            export_names: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DataInitializer {
    pub memory_index: MemoryIndex,
    pub base: Option<GlobalIndex>,
    pub offset: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct TableInitializer {
    pub table_index: TableIndex,
    pub base: Option<GlobalIndex>,
    pub offset: u32,
    pub elements: Vec<FuncIndex>,
}

#[derive(Clone)]
pub struct NixModuleInfo {
    pub(crate) config: TargetFrontendConfig,

    pub signatures: PrimaryMap<TypeIndex, ir::Signature>,

    pub imported_funcs: Vec<(String, String)>,
    pub imported_globals: Vec<(String, String)>,
    pub imported_memories: Vec<(String, String)>,
    pub imported_tables: Vec<(String, String)>,

    pub functions: PrimaryMap<FuncIndex, Exportable<TypeIndex>>,
    pub function_bodies: PrimaryMap<DefinedFuncIndex, ir::Function>,

    pub memories: PrimaryMap<MemoryIndex, Exportable<Memory>>,
    pub tables: PrimaryMap<TableIndex, Exportable<Table>>,
    pub globals: PrimaryMap<GlobalIndex, Exportable<Global>>,

    pub global_inits: SecondaryMap<GlobalIndex, Option<ConstExpr>>,

    pub data_segments: SecondaryMap<DataIndex, Vec<u8>>,
    pub data_initializers: Vec<DataInitializer>,
    pub table_initializers: Vec<TableInitializer>,
    pub element_segments: SecondaryMap<ElemIndex, Vec<FuncIndex>>,

    data_segment_count: usize,
    elem_segment_count: usize,
    pub start_func: Option<FuncIndex>,
}

impl NixModuleInfo {
    fn new(config: TargetFrontendConfig) -> Self {
        Self {
            config,
            signatures: PrimaryMap::new(),
            imported_funcs: Vec::new(),
            imported_globals: Vec::new(),
            imported_memories: Vec::new(),
            imported_tables: Vec::new(),
            functions: PrimaryMap::new(),
            function_bodies: PrimaryMap::new(),
            memories: PrimaryMap::new(),
            tables: PrimaryMap::new(),
            globals: PrimaryMap::new(),
            global_inits: SecondaryMap::new(),
            data_segments: SecondaryMap::new(),
            data_initializers: Vec::new(),
            table_initializers: Vec::new(),
            element_segments: SecondaryMap::new(),
            data_segment_count: 0,
            elem_segment_count: 0,
            start_func: None,
        }
    }

    /// The first type index with the same signature as `index`, so that
    /// `call_indirect` compares structurally equal types as equal.
    pub(crate) fn canonical_type_index(&self, index: TypeIndex) -> TypeIndex {
        let target = &self.signatures[index];
        for (candidate_index, candidate) in &self.signatures {
            if candidate_index == index {
                break;
            }
            if candidate.params == target.params && candidate.returns == target.returns {
                return candidate_index;
            }
        }
        index
    }
}

pub struct NixModule {
    pub info: NixModuleInfo,
    trans: FuncTranslator,
}

impl NixModule {
    fn new() -> Self {
        let config = default_frontend_config();
        Self {
            info: NixModuleInfo::new(config),
            trans: FuncTranslator::new(),
        }
    }

    /// Translates a Wasm module into Cranelift-backed module metadata.
    ///
    /// # Errors
    ///
    /// Returns an error when the input bytes are malformed or contain Wasm
    /// features this translator does not support.
    pub fn translate(wasm: &[u8]) -> Result<Self, TranslateError> {
        let mut module = Self::new();
        translate_module(wasm, &mut module).context(TranslateSnafu)?;
        Ok(module)
    }

    /// The index of a function with a body; imported functions come first.
    pub(crate) fn func_index(&self, defined: DefinedFuncIndex) -> FuncIndex {
        FuncIndex::new(self.info.imported_funcs.len() + defined.index())
    }
}

impl_target_environment!(NixModule, |module| module.info.config);

impl<'data> ModuleEnvironment<'data> for NixModule {
    fn declare_type_func(&mut self, wasm: WasmFuncType) -> WasmResult<()> {
        let mut sig = ir::Signature::new(CallConv::Fast);
        let param = |ty: &WasmValType| clif_type(*ty).map(ir::AbiParam::new);
        sig.params = wasm.params().iter().map(param).collect::<WasmResult<_>>()?;
        sig.returns = wasm
            .returns()
            .iter()
            .map(param)
            .collect::<WasmResult<_>>()?;
        self.info.signatures.push(sig);
        Ok(())
    }

    fn declare_func_import(
        &mut self, index: TypeIndex, module: &'data str, field: &'data str,
    ) -> WasmResult<()> {
        assert_eq!(
            self.info.functions.len(),
            self.info.imported_funcs.len(),
            "Imported functions must be declared first"
        );
        self.info.functions.push(Exportable::new(index));
        self.info
            .imported_funcs
            .push((module.to_string(), field.to_string()));
        Ok(())
    }

    fn declare_table_import(
        &mut self, table: Table, module: &'data str, field: &'data str,
    ) -> WasmResult<()> {
        self.info.tables.push(Exportable::new(table));
        self.info
            .imported_tables
            .push((module.to_string(), field.to_string()));
        Ok(())
    }

    fn declare_memory_import(
        &mut self, memory: Memory, module: &'data str, field: &'data str,
    ) -> WasmResult<()> {
        validate_memory_type(memory)?;
        self.info.memories.push(Exportable::new(memory));
        self.info
            .imported_memories
            .push((module.to_string(), field.to_string()));
        Ok(())
    }

    fn declare_global_import(
        &mut self, global: Global, module: &'data str, field: &'data str,
    ) -> WasmResult<()> {
        validate_global_type(global)?;
        self.info.globals.push(Exportable::new(global));
        self.info
            .imported_globals
            .push((module.to_string(), field.to_string()));
        Ok(())
    }

    fn declare_func_type(&mut self, index: TypeIndex) -> WasmResult<()> {
        self.info.functions.push(Exportable::new(index));
        Ok(())
    }

    fn declare_table(&mut self, table: Table) -> WasmResult<()> {
        self.info.tables.push(Exportable::new(table));
        Ok(())
    }

    fn declare_memory(&mut self, memory: Memory) -> WasmResult<()> {
        validate_memory_type(memory)?;
        self.info.memories.push(Exportable::new(memory));
        Ok(())
    }

    fn declare_global(&mut self, global: Global, init: ConstExpr) -> WasmResult<()> {
        validate_global_type(global)?;
        let idx = self.info.globals.push(Exportable::new(global));
        self.info.global_inits[idx] = Some(init);
        Ok(())
    }

    fn declare_func_export(&mut self, func_index: FuncIndex, name: &'data str) -> WasmResult<()> {
        self.info.functions[func_index]
            .export_names
            .push(name.to_string());
        Ok(())
    }

    fn declare_table_export(
        &mut self, table_index: TableIndex, name: &'data str,
    ) -> WasmResult<()> {
        self.info.tables[table_index]
            .export_names
            .push(name.to_string());
        Ok(())
    }

    fn declare_memory_export(
        &mut self, memory_index: MemoryIndex, name: &'data str,
    ) -> WasmResult<()> {
        self.info.memories[memory_index]
            .export_names
            .push(name.to_string());
        Ok(())
    }

    fn declare_global_export(
        &mut self, global_index: GlobalIndex, name: &'data str,
    ) -> WasmResult<()> {
        self.info.globals[global_index]
            .export_names
            .push(name.to_string());
        Ok(())
    }

    fn declare_start_func(&mut self, index: FuncIndex) -> WasmResult<()> {
        self.info.start_func = Some(index);
        Ok(())
    }

    fn declare_table_elements(
        &mut self, table_index: TableIndex, base: Option<GlobalIndex>, offset: u32,
        elements: Box<[FuncIndex]>,
    ) -> WasmResult<()> {
        let elem_index = ElemIndex::new(self.info.elem_segment_count);
        self.info.elem_segment_count += 1;
        let elements = elements.into_vec();
        // Active element segments are consumed during instantiation. Keep their
        // index present but empty so a later table.init with that index
        // traps on segment bounds.
        self.info.element_segments[elem_index] = Vec::new();
        self.info.table_initializers.push(TableInitializer {
            table_index,
            base,
            offset,
            elements,
        });
        Ok(())
    }

    fn declare_passive_element(
        &mut self, index: ElemIndex, elements: Box<[FuncIndex]>,
    ) -> WasmResult<()> {
        self.info.element_segments[index] = elements.into_vec();
        self.info.elem_segment_count = self.info.elem_segment_count.max(index.index() + 1);
        Ok(())
    }

    fn declare_elements(&mut self, _elements: Box<[FuncIndex]>) -> WasmResult<()> {
        let elem_index = ElemIndex::new(self.info.elem_segment_count);
        self.info.elem_segment_count += 1;
        self.info.element_segments[elem_index] = Vec::new();
        Ok(())
    }

    fn declare_passive_data(&mut self, data_index: DataIndex, data: &'data [u8]) -> WasmResult<()> {
        self.info.data_segments[data_index] = data.to_vec();
        self.info.data_segment_count = self.info.data_segment_count.max(data_index.index() + 1);
        Ok(())
    }

    fn define_function_body(
        &mut self,
        mut validator: cranelift_wasm::wasmparser::FuncValidator<
            cranelift_wasm::wasmparser::ValidatorResources,
        >,
        body: cranelift_wasm::wasmparser::FunctionBody<'data>,
    ) -> WasmResult<()> {
        let func_index = self.func_index(self.info.function_bodies.next_key());
        let sigidx = self.info.functions[func_index].entity;
        let mut func_environ = NixFuncEnvironment::new(&self.info);
        let sig = func_environ.vmctx_sig(sigidx);

        let name = user_name(UserName::Func(func_index))?;
        let mut func = ir::Function::with_name_signature(UserFuncName::User(name), sig);
        self.trans
            .translate_body(&mut validator, body, &mut func, &mut func_environ)?;
        self.info.function_bodies.push(func);
        Ok(())
    }

    fn declare_data_initialization(
        &mut self, memory_index: MemoryIndex, base: Option<GlobalIndex>, offset: u64,
        data: &'data [u8],
    ) -> WasmResult<()> {
        let data_index = DataIndex::new(self.info.data_segment_count);
        self.info.data_segment_count += 1;
        // Like active element segments, active data segments are dropped by
        // instantiation, so a later memory.init with that index traps.
        self.info.data_segments[data_index] = Vec::new();
        self.info.data_initializers.push(DataInitializer {
            memory_index,
            base,
            offset,
            data: data.to_vec(),
        });
        Ok(())
    }

    fn wasm_features(&self) -> wasmparser::WasmFeatures {
        WASM_FEATURES
    }
}

/// Bytes per Wasm memory page.
pub(crate) const WASM_PAGE_SIZE: u64 = 65536;

/// The most pages a 32-bit memory can have.
pub(crate) const WASM32_MAX_PAGES: u64 = 65536;

/// The Wasm proposals the translator and the runtime support.
pub const WASM_FEATURES: wasmparser::WasmFeatures = wasmparser::WasmFeatures::WASM1
    .union(wasmparser::WasmFeatures::MUTABLE_GLOBAL)
    .union(wasmparser::WasmFeatures::SIGN_EXTENSION)
    .union(wasmparser::WasmFeatures::MULTI_VALUE)
    .union(wasmparser::WasmFeatures::REFERENCE_TYPES)
    .union(wasmparser::WasmFeatures::SATURATING_FLOAT_TO_INT)
    .union(wasmparser::WasmFeatures::BULK_MEMORY);

/// The Cranelift type a Wasm value lowers to; references are i32 handles.
pub(crate) fn clif_type(ty: WasmValType) -> WasmResult<ir::Type> {
    match ty {
        WasmValType::I32 | WasmValType::Ref(_) => Ok(I32),
        WasmValType::I64 => Ok(I64),
        WasmValType::F32 => Ok(F32),
        WasmValType::F64 => Ok(F64),
        WasmValType::V128 => Err(WasmError::Unsupported("SIMD/v128".into())),
    }
}

pub(crate) fn validate_memory_type(memory: Memory) -> WasmResult<()> {
    if memory.shared {
        return Err(WasmError::Unsupported("shared memory/threads".into()));
    }
    if memory.memory64 {
        return Err(WasmError::Unsupported("memory64".into()));
    }
    if memory.page_size_log2 != Memory::DEFAULT_PAGE_SIZE_LOG2 {
        return Err(WasmError::Unsupported("custom page sizes".into()));
    }
    Ok(())
}

fn validate_global_type(global: Global) -> WasmResult<()> {
    clif_type(global.wasm_ty).map(drop)
}

/// Implements `TypeConvert` and `TargetEnvironment` for an environment whose
/// `TargetFrontendConfig` `$config` reads from `self`.
macro_rules! impl_target_environment {
    ($ty:ty, | $this:ident | $config:expr) => {
        impl cranelift_wasm::TypeConvert for $ty {
            fn lookup_heap_type(
                &self, _index: cranelift_wasm::wasmparser::UnpackedIndex,
            ) -> cranelift_wasm::WasmHeapType {
                unreachable!("concrete heap types are rejected by WASM_FEATURES")
            }

            fn lookup_type_index(
                &self, _index: cranelift_wasm::wasmparser::UnpackedIndex,
            ) -> cranelift_wasm::EngineOrModuleTypeIndex {
                unreachable!("concrete heap types are rejected by WASM_FEATURES")
            }
        }

        impl cranelift_wasm::TargetEnvironment for $ty {
            fn target_config(&self) -> cranelift_codegen::isa::TargetFrontendConfig {
                let $this = self;
                $config
            }

            fn heap_access_spectre_mitigation(&self) -> bool {
                false
            }

            fn proof_carrying_code(&self) -> bool {
                false
            }

            /// References are i32 table entry handles.
            fn reference_type(
                &self, _ty: cranelift_wasm::WasmHeapType,
            ) -> (cranelift_codegen::ir::Type, bool) {
                (cranelift_codegen::ir::types::I32, false)
            }
        }
    };
}
pub(crate) use impl_target_environment;

const fn default_frontend_config() -> TargetFrontendConfig {
    TargetFrontendConfig {
        default_call_conv: CallConv::Fast,
        pointer_width: PointerWidth::U32,
        page_size_align_log2: Memory::DEFAULT_PAGE_SIZE_LOG2,
    }
}
