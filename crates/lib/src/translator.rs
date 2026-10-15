use cranelift_codegen::{
    ir,
    ir::{
        UserFuncName,
        types::{F32, F64, I32, I64},
    },
    isa::{CallConv, TargetFrontendConfig},
};
use cranelift_entity::{EntityRef, PrimaryMap, SecondaryMap};
use cranelift_wasm::{
    ConstExpr, DataIndex, DefinedFuncIndex, ElemIndex, FuncIndex, FuncTranslator, Global,
    GlobalIndex, Memory, MemoryIndex, ModuleEnvironment, Table, TableIndex, TargetEnvironment,
    TypeConvert, TypeIndex, WasmError, WasmFuncType, WasmHeapType, WasmResult, WasmValType,
    translate_module,
};
use target_lexicon::PointerWidth;
use wasm2nix_runtime::intrinsics as intr;

use crate::func_env::NixFuncEnvironment;

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
    pub data_index: DataIndex,
    pub memory_index: MemoryIndex,
    pub base: Option<GlobalIndex>,
    pub offset: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct TableInitializer {
    pub elem_index: ElemIndex,
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

    pub passive_data: SecondaryMap<DataIndex, Vec<u8>>,
    pub data_segments: SecondaryMap<DataIndex, Vec<u8>>,
    pub data_initializers: Vec<DataInitializer>,
    pub table_initializers: Vec<TableInitializer>,
    pub passive_elements: SecondaryMap<ElemIndex, Vec<FuncIndex>>,
    pub element_segments: SecondaryMap<ElemIndex, Vec<FuncIndex>>,

    data_segment_count: usize,
    elem_segment_count: usize,
    pub start_func: Option<FuncIndex>,
}

impl NixModuleInfo {
    #[must_use]
    pub fn new(config: TargetFrontendConfig) -> Self {
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
            passive_data: SecondaryMap::new(),
            data_segments: SecondaryMap::new(),
            data_initializers: Vec::new(),
            table_initializers: Vec::new(),
            passive_elements: SecondaryMap::new(),
            element_segments: SecondaryMap::new(),
            data_segment_count: 0,
            elem_segment_count: 0,
            start_func: None,
        }
    }

    #[must_use]
    pub fn canonical_type_index(&self, index: TypeIndex) -> TypeIndex {
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

    pub module_name: Option<String>,
    pub function_names: SecondaryMap<FuncIndex, Option<String>>,
}

impl Default for NixModule {
    fn default() -> Self {
        Self::new()
    }
}

impl NixModule {
    #[must_use]
    pub fn new() -> Self {
        let config = default_frontend_config();
        Self {
            info: NixModuleInfo::new(config),
            trans: FuncTranslator::new(),
            module_name: None,
            function_names: SecondaryMap::new(),
        }
    }

    /// Translates a Wasm module into Cranelift-backed module metadata.
    ///
    /// # Errors
    ///
    /// Returns an error when the input bytes are malformed or contain Wasm
    /// features this translator does not support.
    pub fn translate(wasm: &[u8]) -> WasmResult<Self> {
        let mut module = Self::new();
        translate_module(wasm, &mut module)?;
        Ok(module)
    }

    #[must_use]
    pub fn get_func_type(&self, func_index: FuncIndex) -> TypeIndex {
        self.info.functions[func_index].entity
    }

    #[must_use]
    pub const fn num_func_imports(&self) -> usize {
        self.info.imported_funcs.len()
    }
}

impl TypeConvert for NixModule {
    fn lookup_heap_type(&self, _index: cranelift_wasm::wasmparser::UnpackedIndex) -> WasmHeapType {
        unimplemented!("GC/reference types not supported in wasm2nix v0")
    }

    fn lookup_type_index(
        &self, _index: cranelift_wasm::wasmparser::UnpackedIndex,
    ) -> cranelift_wasm::EngineOrModuleTypeIndex {
        unimplemented!("GC/reference types not supported in wasm2nix v0")
    }
}

impl TargetEnvironment for NixModule {
    fn target_config(&self) -> TargetFrontendConfig {
        self.info.config
    }

    fn heap_access_spectre_mitigation(&self) -> bool {
        false
    }

    fn proof_carrying_code(&self) -> bool {
        false
    }

    fn reference_type(&self, _ty: WasmHeapType) -> (ir::Type, bool) {
        (I32, false)
    }
}

impl<'data> ModuleEnvironment<'data> for NixModule {
    fn declare_type_func(&mut self, wasm: WasmFuncType) -> WasmResult<()> {
        let mut sig = ir::Signature::new(CallConv::Fast);

        let mut cvt = |ty: &WasmValType| -> WasmResult<ir::AbiParam> {
            let clif_ty = match ty {
                WasmValType::I32 | WasmValType::Ref(_) => I32,
                WasmValType::I64 => I64,
                WasmValType::F32 => F32,
                WasmValType::F64 => F64,
                WasmValType::V128 => return Err(WasmError::Unsupported("SIMD/v128".into())),
            };
            Ok(ir::AbiParam::new(clif_ty))
        };

        sig.params.extend(
            wasm.params()
                .iter()
                .map(&mut cvt)
                .collect::<WasmResult<Vec<_>>>()?,
        );
        sig.returns.extend(
            wasm.returns()
                .iter()
                .map(&mut cvt)
                .collect::<WasmResult<Vec<_>>>()?,
        );
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

    fn reserve_table_elements(&mut self, num: u32) -> WasmResult<()> {
        if self.info.passive_elements.is_empty() {
            self.info.passive_elements = SecondaryMap::with_capacity(num as usize);
        }
        if self.info.element_segments.is_empty() {
            self.info.element_segments = SecondaryMap::with_capacity(num as usize);
        }
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
            elem_index,
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
        let elements = elements.into_vec();
        self.info.passive_elements[index].clone_from(&elements);
        self.info.element_segments[index] = elements;
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
        self.info.passive_data[data_index] = data.to_vec();
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
        let func_index = FuncIndex::new(self.num_func_imports() + self.info.function_bodies.len());

        let sigidx = self.get_func_type(func_index);
        let mut func_environ = NixFuncEnvironment::new(&self.info);
        let sig = func_environ.vmctx_sig(sigidx);

        let mut func = ir::Function::with_name_signature(
            UserFuncName::user(intr::UserNamespace::Func.into(), func_index.as_u32()),
            sig,
        );
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
        self.info.data_segments[data_index] = data.to_vec();
        self.info.data_initializers.push(DataInitializer {
            data_index,
            memory_index,
            base,
            offset,
            data: data.to_vec(),
        });
        Ok(())
    }

    fn declare_module_name(&mut self, name: &'data str) {
        self.module_name = Some(name.to_string());
    }

    fn declare_func_name(&mut self, func_index: FuncIndex, name: &'data str) {
        self.function_names[func_index] = Some(name.to_string());
    }

    fn reserve_function_bodies(&mut self, bodies: u32, _code_section_offset: u64) {
        if self.info.function_bodies.is_empty() {
            self.info.function_bodies = PrimaryMap::with_capacity(bodies as usize);
        }
    }

    fn reserve_data_initializers(&mut self, num: u32) -> WasmResult<()> {
        self.info.data_initializers.reserve(num as usize);
        Ok(())
    }

    fn reserve_passive_data(&mut self, count: u32) -> WasmResult<()> {
        if self.info.passive_data.is_empty() {
            self.info.passive_data = SecondaryMap::with_capacity(count as usize);
        }
        if self.info.data_segments.is_empty() {
            self.info.data_segments = SecondaryMap::with_capacity(count as usize);
        }
        Ok(())
    }

    fn wasm_features(&self) -> wasmparser::WasmFeatures {
        wasmparser::WasmFeatures::WASM1
            .union(wasmparser::WasmFeatures::MUTABLE_GLOBAL)
            .union(wasmparser::WasmFeatures::SIGN_EXTENSION)
            .union(wasmparser::WasmFeatures::MULTI_VALUE)
            .union(wasmparser::WasmFeatures::REFERENCE_TYPES)
            .union(wasmparser::WasmFeatures::SATURATING_FLOAT_TO_INT)
            .union(wasmparser::WasmFeatures::BULK_MEMORY)
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
    match global.wasm_ty {
        WasmValType::I32
        | WasmValType::I64
        | WasmValType::F32
        | WasmValType::F64
        | WasmValType::Ref(_) => Ok(()),
        WasmValType::V128 => Err(WasmError::Unsupported("SIMD/v128".into())),
    }
}

const fn default_frontend_config() -> TargetFrontendConfig {
    TargetFrontendConfig {
        default_call_conv: CallConv::Fast,
        pointer_width: PointerWidth::U32,
        page_size_align_log2: Memory::DEFAULT_PAGE_SIZE_LOG2,
    }
}
