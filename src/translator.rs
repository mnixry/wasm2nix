use crate::intrinsics as intr;
use cranelift_codegen::cursor::FuncCursor;
use cranelift_codegen::ir;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::immediates::Imm64;
use cranelift_codegen::ir::types::{F32, F64, I32, I64};
use cranelift_codegen::ir::{InstBuilder, UserFuncName};
use cranelift_codegen::isa::{CallConv, TargetFrontendConfig};
use cranelift_entity::{EntityRef, PrimaryMap, SecondaryMap};
use cranelift_frontend::FunctionBuilder;
use cranelift_wasm::{
    ConstExpr, DataIndex, DefinedFuncIndex, ElemIndex, FuncEnvironment, FuncIndex, FuncTranslator,
    Global, GlobalIndex, GlobalVariable, Heap, HeapData, HeapStyle, Memory, MemoryIndex,
    ModuleEnvironment, Table, TableIndex, TargetEnvironment, TypeConvert, TypeIndex,
    WASM32_MAX_SIZE, WasmError, WasmFuncType, WasmHeapType, WasmResult, WasmValType,
    translate_module,
};

#[derive(Debug, Clone)]
pub struct Exportable<T> {
    pub entity: T,
    pub export_names: Vec<String>,
}

impl<T> Exportable<T> {
    fn new(entity: T) -> Self {
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
    config: TargetFrontendConfig,

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

    pub fn canonical_type_index(&self, index: TypeIndex) -> TypeIndex {
        let target = &self.signatures[index];
        for (candidate_index, candidate) in self.signatures.iter() {
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
    pub fn new() -> Self {
        let config = default_frontend_config();
        Self {
            info: NixModuleInfo::new(config),
            trans: FuncTranslator::new(),
            module_name: None,
            function_names: SecondaryMap::new(),
        }
    }

    pub fn translate(wasm: &[u8]) -> WasmResult<Self> {
        let mut module = Self::new();
        translate_module(wasm, &mut module)?;
        Ok(module)
    }

    pub fn get_func_type(&self, func_index: FuncIndex) -> TypeIndex {
        self.info.functions[func_index].entity
    }

    pub fn num_func_imports(&self) -> usize {
        self.info.imported_funcs.len()
    }
}

impl TypeConvert for NixModule {
    fn lookup_heap_type(&self, _index: cranelift_wasm::wasmparser::UnpackedIndex) -> WasmHeapType {
        unimplemented!("GC/reference types not supported in wasm2nix v0")
    }

    fn lookup_type_index(
        &self,
        _index: cranelift_wasm::wasmparser::UnpackedIndex,
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
        (self.pointer_type(), false)
    }
}

impl<'data> ModuleEnvironment<'data> for NixModule {
    fn declare_type_func(&mut self, wasm: WasmFuncType) -> WasmResult<()> {
        let mut sig = ir::Signature::new(CallConv::Fast);

        let mut cvt = |ty: &WasmValType| -> WasmResult<ir::AbiParam> {
            let clif_ty = match ty {
                WasmValType::I32 => I32,
                WasmValType::I64 => I64,
                WasmValType::F32 => F32,
                WasmValType::F64 => F64,
                WasmValType::V128 => return Err(WasmError::Unsupported("SIMD/v128".into())),
                WasmValType::Ref(_) => I32,
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
        &mut self,
        index: TypeIndex,
        module: &'data str,
        field: &'data str,
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
        &mut self,
        table: Table,
        module: &'data str,
        field: &'data str,
    ) -> WasmResult<()> {
        self.info.tables.push(Exportable::new(table));
        self.info
            .imported_tables
            .push((module.to_string(), field.to_string()));
        Ok(())
    }

    fn declare_memory_import(
        &mut self,
        memory: Memory,
        module: &'data str,
        field: &'data str,
    ) -> WasmResult<()> {
        validate_memory_type(memory)?;
        self.info.memories.push(Exportable::new(memory));
        self.info
            .imported_memories
            .push((module.to_string(), field.to_string()));
        Ok(())
    }

    fn declare_global_import(
        &mut self,
        global: Global,
        module: &'data str,
        field: &'data str,
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
        &mut self,
        table_index: TableIndex,
        name: &'data str,
    ) -> WasmResult<()> {
        self.info.tables[table_index]
            .export_names
            .push(name.to_string());
        Ok(())
    }

    fn declare_memory_export(
        &mut self,
        memory_index: MemoryIndex,
        name: &'data str,
    ) -> WasmResult<()> {
        self.info.memories[memory_index]
            .export_names
            .push(name.to_string());
        Ok(())
    }

    fn declare_global_export(
        &mut self,
        global_index: GlobalIndex,
        name: &'data str,
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
        &mut self,
        table_index: TableIndex,
        base: Option<GlobalIndex>,
        offset: u32,
        elements: Box<[FuncIndex]>,
    ) -> WasmResult<()> {
        let elem_index = ElemIndex::new(self.info.elem_segment_count);
        self.info.elem_segment_count += 1;
        let elements = elements.into_vec();
        // Active element segments are consumed during instantiation. Keep their index present
        // but empty so a later table.init with that index traps on segment bounds.
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
        &mut self,
        index: ElemIndex,
        elements: Box<[FuncIndex]>,
    ) -> WasmResult<()> {
        let elements = elements.into_vec();
        self.info.passive_elements[index] = elements.clone();
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
            UserFuncName::user(intr::FUNC_NAMESPACE, func_index.as_u32()),
            sig,
        );
        self.trans
            .translate_body(&mut validator, body, &mut func, &mut func_environ)?;
        self.info.function_bodies.push(func);
        Ok(())
    }

    fn declare_data_initialization(
        &mut self,
        memory_index: MemoryIndex,
        base: Option<GlobalIndex>,
        offset: u64,
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

    fn wasm_features(&self) -> cranelift_wasm::wasmparser::WasmFeatures {
        cranelift_wasm::wasmparser::WasmFeatures::WASM1
            .union(cranelift_wasm::wasmparser::WasmFeatures::MUTABLE_GLOBAL)
            .union(cranelift_wasm::wasmparser::WasmFeatures::SIGN_EXTENSION)
            .union(cranelift_wasm::wasmparser::WasmFeatures::MULTI_VALUE)
            .union(cranelift_wasm::wasmparser::WasmFeatures::REFERENCE_TYPES)
            .union(cranelift_wasm::wasmparser::WasmFeatures::SATURATING_FLOAT_TO_INT)
            .union(cranelift_wasm::wasmparser::WasmFeatures::BULK_MEMORY)
    }
}

fn validate_memory_type(memory: Memory) -> WasmResult<()> {
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

#[derive(Debug, Default, Clone)]
struct Intrinsics {
    mem_grow: Option<ir::FuncRef>,
    mem_size: Option<ir::FuncRef>,
    mem_copy: Option<ir::FuncRef>,
    mem_fill: Option<ir::FuncRef>,
    mem_init: Option<ir::FuncRef>,
    data_drop: Option<ir::FuncRef>,
    table_copy: Option<ir::FuncRef>,
    table_init: Option<ir::FuncRef>,
    elem_drop: Option<ir::FuncRef>,
    table_get: Option<ir::FuncRef>,
    table_set: Option<ir::FuncRef>,

    global_get_i32: Option<ir::FuncRef>,
    global_get_i64: Option<ir::FuncRef>,
    global_get_f32: Option<ir::FuncRef>,
    global_get_f64: Option<ir::FuncRef>,

    global_set_i32: Option<ir::FuncRef>,
    global_set_i64: Option<ir::FuncRef>,
    global_set_f32: Option<ir::FuncRef>,
    global_set_f64: Option<ir::FuncRef>,
}

pub struct NixFuncEnvironment<'m> {
    mod_info: &'m NixModuleInfo,
    heaps: PrimaryMap<Heap, HeapData>,
    intrinsics: Intrinsics,
}

impl<'m> NixFuncEnvironment<'m> {
    pub fn new(mod_info: &'m NixModuleInfo) -> Self {
        Self {
            mod_info,
            heaps: Default::default(),
            intrinsics: Intrinsics::default(),
        }
    }

    pub fn vmctx_sig(&self, sigidx: TypeIndex) -> ir::Signature {
        let mut sig = self.mod_info.signatures[sigidx].clone();
        sig.params.push(ir::AbiParam::special(
            self.pointer_type(),
            ir::ArgumentPurpose::VMContext,
        ));
        sig
    }

    fn import_intrinsic(
        &mut self,
        func: &mut ir::Function,
        intrinsic_index: u32,
        sig: ir::Signature,
    ) -> ir::FuncRef {
        let signature = func.import_signature(sig);
        let name =
            ir::ExternalName::User(func.declare_imported_user_function(ir::UserExternalName {
                namespace: intr::INTRINSIC_NAMESPACE,
                index: intrinsic_index,
            }));
        func.import_function(ir::ExtFuncData {
            name,
            signature,
            colocated: false,
        })
    }

    fn intrinsic_mem_grow(&mut self, func: &mut ir::Function) -> ir::FuncRef {
        if let Some(f) = self.intrinsics.mem_grow {
            return f;
        }
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::special(
            self.pointer_type(),
            ir::ArgumentPurpose::VMContext,
        ));
        sig.returns.push(ir::AbiParam::new(I32));

        let fref = self.import_intrinsic(func, intr::MEM_GROW, sig);
        self.intrinsics.mem_grow = Some(fref);
        fref
    }

    fn intrinsic_mem_size(&mut self, func: &mut ir::Function) -> ir::FuncRef {
        if let Some(f) = self.intrinsics.mem_size {
            return f;
        }
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.params.push(ir::AbiParam::special(
            self.pointer_type(),
            ir::ArgumentPurpose::VMContext,
        ));
        sig.returns.push(ir::AbiParam::new(I32));

        let fref = self.import_intrinsic(func, intr::MEM_SIZE, sig);
        self.intrinsics.mem_size = Some(fref);
        fref
    }

    fn intrinsic_mem_copy(&mut self, func: &mut ir::Function) -> ir::FuncRef {
        if let Some(f) = self.intrinsics.mem_copy {
            return f;
        }
        let ptr = self.pointer_type();
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params
            .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
        let fref = self.import_intrinsic(func, intr::MEM_COPY, sig);
        self.intrinsics.mem_copy = Some(fref);
        fref
    }

    fn intrinsic_mem_fill(&mut self, func: &mut ir::Function) -> ir::FuncRef {
        if let Some(f) = self.intrinsics.mem_fill {
            return f;
        }
        let ptr = self.pointer_type();
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params
            .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
        let fref = self.import_intrinsic(func, intr::MEM_FILL, sig);
        self.intrinsics.mem_fill = Some(fref);
        fref
    }

    fn intrinsic_mem_init(&mut self, func: &mut ir::Function) -> ir::FuncRef {
        if let Some(f) = self.intrinsics.mem_init {
            return f;
        }
        let ptr = self.pointer_type();
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params
            .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
        let fref = self.import_intrinsic(func, intr::MEM_INIT, sig);
        self.intrinsics.mem_init = Some(fref);
        fref
    }

    fn intrinsic_data_drop(&mut self, func: &mut ir::Function) -> ir::FuncRef {
        if let Some(f) = self.intrinsics.data_drop {
            return f;
        }
        let ptr = self.pointer_type();
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.params.push(ir::AbiParam::new(I32));
        sig.params
            .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
        let fref = self.import_intrinsic(func, intr::DATA_DROP, sig);
        self.intrinsics.data_drop = Some(fref);
        fref
    }

    fn intrinsic_table_copy(&mut self, func: &mut ir::Function) -> ir::FuncRef {
        if let Some(f) = self.intrinsics.table_copy {
            return f;
        }
        let ptr = self.pointer_type();
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params
            .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
        let fref = self.import_intrinsic(func, intr::TABLE_COPY, sig);
        self.intrinsics.table_copy = Some(fref);
        fref
    }

    fn intrinsic_table_init(&mut self, func: &mut ir::Function) -> ir::FuncRef {
        if let Some(f) = self.intrinsics.table_init {
            return f;
        }
        let ptr = self.pointer_type();
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params
            .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
        let fref = self.import_intrinsic(func, intr::TABLE_INIT, sig);
        self.intrinsics.table_init = Some(fref);
        fref
    }

    fn intrinsic_elem_drop(&mut self, func: &mut ir::Function) -> ir::FuncRef {
        if let Some(f) = self.intrinsics.elem_drop {
            return f;
        }
        let ptr = self.pointer_type();
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.params.push(ir::AbiParam::new(I32));
        sig.params
            .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
        let fref = self.import_intrinsic(func, intr::ELEM_DROP, sig);
        self.intrinsics.elem_drop = Some(fref);
        fref
    }

    fn intrinsic_table_get(&mut self, func: &mut ir::Function) -> ir::FuncRef {
        if let Some(f) = self.intrinsics.table_get {
            return f;
        }
        let ptr = self.pointer_type();
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params
            .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
        sig.returns.push(ir::AbiParam::new(I32));
        let fref = self.import_intrinsic(func, intr::TABLE_GET, sig);
        self.intrinsics.table_get = Some(fref);
        fref
    }

    fn intrinsic_table_set(&mut self, func: &mut ir::Function) -> ir::FuncRef {
        if let Some(f) = self.intrinsics.table_set {
            return f;
        }
        let ptr = self.pointer_type();
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params.push(ir::AbiParam::new(I32));
        sig.params
            .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
        let fref = self.import_intrinsic(func, intr::TABLE_SET, sig);
        self.intrinsics.table_set = Some(fref);
        fref
    }

    fn intrinsic_global_get(&mut self, func: &mut ir::Function, ty: ir::Type) -> ir::FuncRef {
        match ty {
            I32 => {
                if let Some(f) = self.intrinsics.global_get_i32 {
                    return f;
                }
                let ptr = self.pointer_type();
                let mut sig = ir::Signature::new(CallConv::Fast);
                sig.params.push(ir::AbiParam::new(I32));
                sig.params
                    .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
                sig.returns.push(ir::AbiParam::new(I32));
                let fref = self.import_intrinsic(func, intr::GLOBAL_GET_I32, sig);
                self.intrinsics.global_get_i32 = Some(fref);
                fref
            }
            I64 => {
                if let Some(f) = self.intrinsics.global_get_i64 {
                    return f;
                }
                let ptr = self.pointer_type();
                let mut sig = ir::Signature::new(CallConv::Fast);
                sig.params.push(ir::AbiParam::new(I32));
                sig.params
                    .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
                sig.returns.push(ir::AbiParam::new(I64));
                let fref = self.import_intrinsic(func, intr::GLOBAL_GET_I64, sig);
                self.intrinsics.global_get_i64 = Some(fref);
                fref
            }
            F32 => {
                if let Some(f) = self.intrinsics.global_get_f32 {
                    return f;
                }
                let ptr = self.pointer_type();
                let mut sig = ir::Signature::new(CallConv::Fast);
                sig.params.push(ir::AbiParam::new(I32));
                sig.params
                    .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
                sig.returns.push(ir::AbiParam::new(F32));
                let fref = self.import_intrinsic(func, intr::GLOBAL_GET_F32, sig);
                self.intrinsics.global_get_f32 = Some(fref);
                fref
            }
            F64 => {
                if let Some(f) = self.intrinsics.global_get_f64 {
                    return f;
                }
                let ptr = self.pointer_type();
                let mut sig = ir::Signature::new(CallConv::Fast);
                sig.params.push(ir::AbiParam::new(I32));
                sig.params
                    .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
                sig.returns.push(ir::AbiParam::new(F64));
                let fref = self.import_intrinsic(func, intr::GLOBAL_GET_F64, sig);
                self.intrinsics.global_get_f64 = Some(fref);
                fref
            }
            _ => unreachable!("unsupported global type {ty}"),
        }
    }

    fn intrinsic_global_set(&mut self, func: &mut ir::Function, ty: ir::Type) -> ir::FuncRef {
        match ty {
            I32 => {
                if let Some(f) = self.intrinsics.global_set_i32 {
                    return f;
                }
                let ptr = self.pointer_type();
                let mut sig = ir::Signature::new(CallConv::Fast);
                sig.params.push(ir::AbiParam::new(I32));
                sig.params.push(ir::AbiParam::new(I32));
                sig.params
                    .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
                let fref = self.import_intrinsic(func, intr::GLOBAL_SET_I32, sig);
                self.intrinsics.global_set_i32 = Some(fref);
                fref
            }
            I64 => {
                if let Some(f) = self.intrinsics.global_set_i64 {
                    return f;
                }
                let ptr = self.pointer_type();
                let mut sig = ir::Signature::new(CallConv::Fast);
                sig.params.push(ir::AbiParam::new(I32));
                sig.params.push(ir::AbiParam::new(I64));
                sig.params
                    .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
                let fref = self.import_intrinsic(func, intr::GLOBAL_SET_I64, sig);
                self.intrinsics.global_set_i64 = Some(fref);
                fref
            }
            F32 => {
                if let Some(f) = self.intrinsics.global_set_f32 {
                    return f;
                }
                let ptr = self.pointer_type();
                let mut sig = ir::Signature::new(CallConv::Fast);
                sig.params.push(ir::AbiParam::new(I32));
                sig.params.push(ir::AbiParam::new(F32));
                sig.params
                    .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
                let fref = self.import_intrinsic(func, intr::GLOBAL_SET_F32, sig);
                self.intrinsics.global_set_f32 = Some(fref);
                fref
            }
            F64 => {
                if let Some(f) = self.intrinsics.global_set_f64 {
                    return f;
                }
                let ptr = self.pointer_type();
                let mut sig = ir::Signature::new(CallConv::Fast);
                sig.params.push(ir::AbiParam::new(I32));
                sig.params.push(ir::AbiParam::new(F64));
                sig.params
                    .push(ir::AbiParam::special(ptr, ir::ArgumentPurpose::VMContext));
                let fref = self.import_intrinsic(func, intr::GLOBAL_SET_F64, sig);
                self.intrinsics.global_set_f64 = Some(fref);
                fref
            }
            _ => unreachable!("unsupported global type {ty}"),
        }
    }

    fn intrinsic_call_indirect(
        &mut self,
        func: &mut ir::Function,
        table_index: TableIndex,
        sig_index: TypeIndex,
    ) -> ir::FuncRef {
        let mut sig = self.vmctx_sig(sig_index);
        sig.params.insert(0, ir::AbiParam::new(I32));
        let canonical_sig_index = self.mod_info.canonical_type_index(sig_index);
        let intrinsic_index = intr::CALL_INDIRECT_BASE
            + table_index.as_u32() * intr::CALL_INDIRECT_STRIDE
            + canonical_sig_index.as_u32();
        self.import_intrinsic(func, intrinsic_index, sig)
    }

    fn wasm_ty_to_clif_ty(ty: WasmValType) -> WasmResult<ir::Type> {
        match ty {
            WasmValType::I32 => Ok(I32),
            WasmValType::I64 => Ok(I64),
            WasmValType::F32 => Ok(F32),
            WasmValType::F64 => Ok(F64),
            WasmValType::V128 => Err(WasmError::Unsupported("SIMD/v128".into())),
            WasmValType::Ref(_) => Ok(I32),
        }
    }
}

impl<'m> TypeConvert for NixFuncEnvironment<'m> {
    fn lookup_heap_type(&self, _index: cranelift_wasm::wasmparser::UnpackedIndex) -> WasmHeapType {
        unimplemented!("GC/reference types not supported in wasm2nix v0")
    }

    fn lookup_type_index(
        &self,
        _index: cranelift_wasm::wasmparser::UnpackedIndex,
    ) -> cranelift_wasm::EngineOrModuleTypeIndex {
        unimplemented!("GC/reference types not supported in wasm2nix v0")
    }
}

impl<'m> TargetEnvironment for NixFuncEnvironment<'m> {
    fn target_config(&self) -> TargetFrontendConfig {
        self.mod_info.config
    }

    fn heap_access_spectre_mitigation(&self) -> bool {
        false
    }

    fn proof_carrying_code(&self) -> bool {
        false
    }

    fn reference_type(&self, _ty: WasmHeapType) -> (ir::Type, bool) {
        (self.pointer_type(), false)
    }
}

impl<'m> FuncEnvironment for NixFuncEnvironment<'m> {
    fn param_needs_stack_map(&self, _signature: &ir::Signature, _index: usize) -> bool {
        false
    }

    fn sig_ref_result_needs_stack_map(&self, _sig_ref: ir::SigRef, _index: usize) -> bool {
        false
    }

    fn func_ref_result_needs_stack_map(
        &self,
        _func: &ir::Function,
        _func_ref: ir::FuncRef,
        _index: usize,
    ) -> bool {
        false
    }

    fn make_global(
        &mut self,
        _func: &mut ir::Function,
        _index: GlobalIndex,
    ) -> WasmResult<GlobalVariable> {
        Ok(GlobalVariable::Custom)
    }

    fn heaps(&self) -> &PrimaryMap<Heap, HeapData> {
        &self.heaps
    }

    fn make_heap(&mut self, func: &mut ir::Function, index: MemoryIndex) -> WasmResult<Heap> {
        // v0: only wasm32 memory index 0.
        if index.index() != 0 {
            return Err(WasmError::Unsupported("multiple memories".into()));
        }

        let memory = self
            .mod_info
            .memories
            .get(index)
            .map(|e| e.entity)
            .ok_or_else(|| WasmError::Unsupported("memory referenced but not declared".into()))?;
        validate_memory_type(memory)?;

        // Make the base address symbolic so `heap_addr` results are stable and can be treated as
        // linear offsets during CLIF→Nix lowering (we'll interpret the base as 0).
        let base_name = func.declare_imported_user_function(ir::UserExternalName {
            namespace: intr::INTRINSIC_NAMESPACE,
            index: 1000 + index.as_u32(),
        });
        let base = func.create_global_value(ir::GlobalValueData::Symbol {
            name: ir::ExternalName::User(base_name),
            offset: Imm64::new(0),
            colocated: false,
            tls: false,
        });

        // Allow addressing up to the wasm32 maximum (4GiB); runtime will enforce actual size.
        let bound = WASM32_MAX_SIZE;

        Ok(self.heaps.push(HeapData {
            base,
            min_size: 0,
            max_size: Some(bound),
            offset_guard_size: 0,
            style: HeapStyle::Static { bound },
            index_type: I32,
            memory_type: None,
            page_size_log2: memory.page_size_log2,
        }))
    }

    fn make_indirect_sig(
        &mut self,
        func: &mut ir::Function,
        index: TypeIndex,
    ) -> WasmResult<ir::SigRef> {
        Ok(func.import_signature(self.vmctx_sig(index)))
    }

    fn make_direct_func(
        &mut self,
        func: &mut ir::Function,
        index: FuncIndex,
    ) -> WasmResult<ir::FuncRef> {
        let sigidx = self.mod_info.functions[index].entity;
        let signature = func.import_signature(self.vmctx_sig(sigidx));
        let name =
            ir::ExternalName::User(func.declare_imported_user_function(ir::UserExternalName {
                namespace: intr::FUNC_NAMESPACE,
                index: index.as_u32(),
            }));
        Ok(func.import_function(ir::ExtFuncData {
            name,
            signature,
            colocated: false,
        }))
    }

    fn translate_call(
        &mut self,
        builder: &mut FunctionBuilder,
        _callee_index: FuncIndex,
        callee: ir::FuncRef,
        call_args: &[ir::Value],
    ) -> WasmResult<ir::Inst> {
        let vmctx = builder
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");

        let mut args = Vec::with_capacity(call_args.len() + 1);
        args.extend(call_args.iter().cloned());
        args.push(vmctx);
        Ok(builder.ins().call(callee, &args))
    }

    fn translate_call_indirect(
        &mut self,
        builder: &mut FunctionBuilder,
        table_index: TableIndex,
        sig_index: TypeIndex,
        _sig_ref: ir::SigRef,
        callee: ir::Value,
        call_args: &[ir::Value],
    ) -> WasmResult<Option<ir::Inst>> {
        let vmctx = builder
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");
        let fref = self.intrinsic_call_indirect(builder.func, table_index, sig_index);
        let mut args = Vec::with_capacity(call_args.len() + 2);
        args.push(callee);
        args.extend(call_args.iter().cloned());
        args.push(vmctx);
        Ok(Some(builder.ins().call(fref, &args)))
    }

    fn translate_return_call_indirect(
        &mut self,
        _builder: &mut FunctionBuilder,
        _table_index: TableIndex,
        _sig_index: TypeIndex,
        _sig_ref: ir::SigRef,
        _callee: ir::Value,
        _call_args: &[ir::Value],
    ) -> WasmResult<()> {
        Err(WasmError::Unsupported("return_call_indirect".into()))
    }

    fn translate_return_call_ref(
        &mut self,
        _builder: &mut FunctionBuilder,
        _sig_ref: ir::SigRef,
        _callee: ir::Value,
        _call_args: &[ir::Value],
    ) -> WasmResult<()> {
        Err(WasmError::Unsupported("return_call_ref".into()))
    }

    fn translate_call_ref(
        &mut self,
        _builder: &mut FunctionBuilder,
        _sig_ref: ir::SigRef,
        _callee: ir::Value,
        _call_args: &[ir::Value],
    ) -> WasmResult<ir::Inst> {
        Err(WasmError::Unsupported("call_ref".into()))
    }

    fn translate_memory_grow(
        &mut self,
        mut pos: FuncCursor,
        index: MemoryIndex,
        _heap: Heap,
        val: ir::Value,
    ) -> WasmResult<ir::Value> {
        if index.index() != 0 {
            return Err(WasmError::Unsupported("multiple memories".into()));
        }
        let vmctx = pos
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");
        let fref = self.intrinsic_mem_grow(pos.func);
        let inst = pos.ins().call(fref, &[val, vmctx]);
        Ok(pos.func.dfg.inst_results(inst)[0])
    }

    fn translate_memory_size(
        &mut self,
        mut pos: FuncCursor,
        index: MemoryIndex,
        _heap: Heap,
    ) -> WasmResult<ir::Value> {
        if index.index() != 0 {
            return Err(WasmError::Unsupported("multiple memories".into()));
        }
        let vmctx = pos
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");
        let fref = self.intrinsic_mem_size(pos.func);
        let inst = pos.ins().call(fref, &[vmctx]);
        Ok(pos.func.dfg.inst_results(inst)[0])
    }

    fn translate_memory_copy(
        &mut self,
        mut pos: FuncCursor,
        src_index: MemoryIndex,
        _src_heap: Heap,
        dst_index: MemoryIndex,
        _dst_heap: Heap,
        dst: ir::Value,
        src: ir::Value,
        len: ir::Value,
    ) -> WasmResult<()> {
        if src_index.index() != 0 || dst_index.index() != 0 {
            return Err(WasmError::Unsupported("multiple memories".into()));
        }
        let vmctx = pos
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");
        let fref = self.intrinsic_mem_copy(pos.func);
        pos.ins().call(fref, &[dst, src, len, vmctx]);
        Ok(())
    }

    fn translate_memory_fill(
        &mut self,
        mut pos: FuncCursor,
        index: MemoryIndex,
        _heap: Heap,
        dst: ir::Value,
        val: ir::Value,
        len: ir::Value,
    ) -> WasmResult<()> {
        if index.index() != 0 {
            return Err(WasmError::Unsupported("multiple memories".into()));
        }
        let vmctx = pos
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");
        let fref = self.intrinsic_mem_fill(pos.func);
        pos.ins().call(fref, &[dst, val, len, vmctx]);
        Ok(())
    }

    fn translate_memory_init(
        &mut self,
        mut pos: FuncCursor,
        index: MemoryIndex,
        _heap: Heap,
        seg_index: u32,
        dst: ir::Value,
        src: ir::Value,
        len: ir::Value,
    ) -> WasmResult<()> {
        if index.index() != 0 {
            return Err(WasmError::Unsupported("multiple memories".into()));
        }
        let vmctx = pos
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");
        let seg = pos.ins().iconst(I32, i64::from(seg_index));
        let fref = self.intrinsic_mem_init(pos.func);
        pos.ins().call(fref, &[seg, dst, src, len, vmctx]);
        Ok(())
    }

    fn translate_data_drop(&mut self, mut pos: FuncCursor, seg_index: u32) -> WasmResult<()> {
        let vmctx = pos
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");
        let seg = pos.ins().iconst(I32, i64::from(seg_index));
        let fref = self.intrinsic_data_drop(pos.func);
        pos.ins().call(fref, &[seg, vmctx]);
        Ok(())
    }

    fn translate_table_size(
        &mut self,
        _pos: FuncCursor,
        _index: TableIndex,
    ) -> WasmResult<ir::Value> {
        Err(WasmError::Unsupported("tables".into()))
    }

    fn translate_table_grow(
        &mut self,
        _pos: FuncCursor,
        _table_index: TableIndex,
        _delta: ir::Value,
        _init_value: ir::Value,
    ) -> WasmResult<ir::Value> {
        Err(WasmError::Unsupported("tables".into()))
    }

    fn translate_table_get(
        &mut self,
        builder: &mut FunctionBuilder,
        table_index: TableIndex,
        index: ir::Value,
    ) -> WasmResult<ir::Value> {
        let vmctx = builder
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");
        let table = builder.ins().iconst(I32, i64::from(table_index.as_u32()));
        let fref = self.intrinsic_table_get(builder.func);
        let call = builder.ins().call(fref, &[table, index, vmctx]);
        Ok(builder.inst_results(call)[0])
    }

    fn translate_table_set(
        &mut self,
        builder: &mut FunctionBuilder,
        table_index: TableIndex,
        value: ir::Value,
        index: ir::Value,
    ) -> WasmResult<()> {
        let vmctx = builder
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");
        let table = builder.ins().iconst(I32, i64::from(table_index.as_u32()));
        let fref = self.intrinsic_table_set(builder.func);
        builder.ins().call(fref, &[table, index, value, vmctx]);
        Ok(())
    }

    fn translate_table_copy(
        &mut self,
        mut pos: FuncCursor,
        dst_table_index: TableIndex,
        src_table_index: TableIndex,
        dst: ir::Value,
        src: ir::Value,
        len: ir::Value,
    ) -> WasmResult<()> {
        let vmctx = pos
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");
        let dst_table = pos.ins().iconst(I32, i64::from(dst_table_index.as_u32()));
        let src_table = pos.ins().iconst(I32, i64::from(src_table_index.as_u32()));
        let fref = self.intrinsic_table_copy(pos.func);
        pos.ins()
            .call(fref, &[dst_table, src_table, dst, src, len, vmctx]);
        Ok(())
    }

    fn translate_table_fill(
        &mut self,
        _pos: FuncCursor,
        _table_index: TableIndex,
        _dst: ir::Value,
        _val: ir::Value,
        _len: ir::Value,
    ) -> WasmResult<()> {
        Err(WasmError::Unsupported("tables".into()))
    }

    fn translate_table_init(
        &mut self,
        mut pos: FuncCursor,
        seg_index: u32,
        table_index: TableIndex,
        dst: ir::Value,
        src: ir::Value,
        len: ir::Value,
    ) -> WasmResult<()> {
        let vmctx = pos
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");
        let table = pos.ins().iconst(I32, i64::from(table_index.as_u32()));
        let seg = pos.ins().iconst(I32, i64::from(seg_index));
        let fref = self.intrinsic_table_init(pos.func);
        pos.ins().call(fref, &[table, seg, dst, src, len, vmctx]);
        Ok(())
    }

    fn translate_elem_drop(&mut self, mut pos: FuncCursor, seg_index: u32) -> WasmResult<()> {
        let vmctx = pos
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");
        let seg = pos.ins().iconst(I32, i64::from(seg_index));
        let fref = self.intrinsic_elem_drop(pos.func);
        pos.ins().call(fref, &[seg, vmctx]);
        Ok(())
    }

    fn translate_ref_null(
        &mut self,
        mut pos: FuncCursor,
        _ty: WasmHeapType,
    ) -> WasmResult<ir::Value> {
        Ok(pos.ins().iconst(I32, 0))
    }

    fn translate_ref_is_null(
        &mut self,
        mut pos: FuncCursor,
        value: ir::Value,
    ) -> WasmResult<ir::Value> {
        let is_null = pos.ins().icmp_imm(IntCC::Equal, value, 0);
        Ok(pos.ins().uextend(I32, is_null))
    }

    fn translate_ref_func(
        &mut self,
        _pos: FuncCursor,
        _func_index: FuncIndex,
    ) -> WasmResult<ir::Value> {
        Err(WasmError::Unsupported("ref.func".into()))
    }

    fn translate_custom_global_get(
        &mut self,
        builder: &mut FunctionBuilder,
        global_index: GlobalIndex,
    ) -> WasmResult<ir::Value> {
        let global = self.mod_info.globals[global_index].entity;
        let clif_ty = Self::wasm_ty_to_clif_ty(global.wasm_ty)?;
        let fref = self.intrinsic_global_get(builder.func, clif_ty);

        let vmctx = builder
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");

        let idx = builder.ins().iconst(I32, i64::from(global_index.as_u32()));
        let call = builder.ins().call(fref, &[idx, vmctx]);
        Ok(builder.inst_results(call)[0])
    }

    fn translate_custom_global_set(
        &mut self,
        builder: &mut FunctionBuilder,
        global_index: GlobalIndex,
        val: ir::Value,
    ) -> WasmResult<()> {
        let global = self.mod_info.globals[global_index].entity;
        let clif_ty = Self::wasm_ty_to_clif_ty(global.wasm_ty)?;
        let fref = self.intrinsic_global_set(builder.func, clif_ty);

        let vmctx = builder
            .func
            .special_param(ir::ArgumentPurpose::VMContext)
            .expect("Missing vmctx parameter");

        let idx = builder.ins().iconst(I32, i64::from(global_index.as_u32()));
        builder.ins().call(fref, &[idx, val, vmctx]);
        Ok(())
    }

    fn translate_atomic_wait(
        &mut self,
        _pos: FuncCursor,
        _index: MemoryIndex,
        _heap: Heap,
        _addr: ir::Value,
        _expected: ir::Value,
        _timeout: ir::Value,
    ) -> WasmResult<ir::Value> {
        Err(WasmError::Unsupported("atomics/threads".into()))
    }

    fn translate_atomic_notify(
        &mut self,
        _pos: FuncCursor,
        _index: MemoryIndex,
        _heap: Heap,
        _addr: ir::Value,
        _count: ir::Value,
    ) -> WasmResult<ir::Value> {
        Err(WasmError::Unsupported("atomics/threads".into()))
    }

    fn translate_ref_i31(&mut self, _pos: FuncCursor, _val: ir::Value) -> WasmResult<ir::Value> {
        Err(WasmError::Unsupported("ref.i31".into()))
    }

    fn translate_i31_get_s(
        &mut self,
        _pos: FuncCursor,
        _i31ref: ir::Value,
    ) -> WasmResult<ir::Value> {
        Err(WasmError::Unsupported("i31.get_s".into()))
    }

    fn translate_i31_get_u(
        &mut self,
        _pos: FuncCursor,
        _i31ref: ir::Value,
    ) -> WasmResult<ir::Value> {
        Err(WasmError::Unsupported("i31.get_u".into()))
    }
}

fn default_frontend_config() -> TargetFrontendConfig {
    let isa_name = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "s390x" => "s390x",
        "riscv64" => "riscv64",
        other => other,
    };

    let builder = cranelift_codegen::isa::lookup_by_name(isa_name)
        .or_else(|_| cranelift_codegen::isa::lookup_by_name("x86_64"))
        .expect("No supported Cranelift ISA backend is enabled");

    let flags = cranelift_codegen::settings::Flags::new(cranelift_codegen::settings::builder());
    let isa = builder
        .finish(flags)
        .expect("Failed to build Cranelift ISA");
    isa.frontend_config()
}
