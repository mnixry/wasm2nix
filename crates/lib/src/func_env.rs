use std::collections::BTreeMap;

use cranelift_codegen::{
    cursor::FuncCursor,
    ir,
    ir::{
        InstBuilder,
        condcodes::IntCC,
        immediates::Imm64,
        types::{F32, F64, I32, I64},
    },
    isa::{CallConv, TargetFrontendConfig},
};
use cranelift_entity::{EntityRef, PrimaryMap};
use cranelift_frontend::FunctionBuilder;
use cranelift_wasm::{
    FuncEnvironment, FuncIndex, GlobalIndex, GlobalVariable, Heap, HeapData, HeapStyle,
    MemoryIndex, TableIndex, TargetEnvironment, TypeConvert, TypeIndex, WASM32_MAX_SIZE, WasmError,
    WasmHeapType, WasmResult,
};
use wasm2nix_runtime::intrinsics as intr;

use crate::translator::{NixModuleInfo, clif_type, validate_memory_type};

macro_rules! intrinsic_abi_param {
    ($env:expr; vmctx) => {
        ir::AbiParam::special($env.pointer_type(), ir::ArgumentPurpose::VMContext)
    };
    ($env:expr; $ty:expr) => {
        ir::AbiParam::new($ty)
    };
}

macro_rules! intrinsic_sig {
    ($env:expr; [$($param:tt),* $(,)?] -> [$($ret:tt),* $(,)?]) => {{
        let mut sig = ir::Signature::new(CallConv::Fast);
        $(sig.params.push(intrinsic_abi_param!($env; $param));)*
        $(sig.returns.push(intrinsic_abi_param!($env; $ret));)*
        sig
    }};
}

macro_rules! cached_intrinsic_method {
    ($method:ident, $intrinsic:path, [$($param:tt),* $(,)?] -> [$($ret:tt),* $(,)?]) => {
        fn $method(&mut self, func: &mut ir::Function) -> ir::FuncRef {
            let sig = intrinsic_sig!(self; [$($param),*] -> [$($ret),*]);
            self.cached_intrinsic(func, $intrinsic, sig)
        }
    };
}

pub struct NixFuncEnvironment<'m> {
    mod_info: &'m NixModuleInfo,
    heaps: PrimaryMap<Heap, HeapData>,
    /// Intrinsic imports already declared in the current function.
    intrinsics: BTreeMap<u32, ir::FuncRef>,
}

impl<'m> NixFuncEnvironment<'m> {
    #[must_use]
    pub fn new(mod_info: &'m NixModuleInfo) -> Self {
        Self {
            mod_info,
            heaps: PrimaryMap::default(),
            intrinsics: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn vmctx_sig(&self, sigidx: TypeIndex) -> ir::Signature {
        let mut sig = self.mod_info.signatures[sigidx].clone();
        sig.params.push(ir::AbiParam::special(
            self.pointer_type(),
            ir::ArgumentPurpose::VMContext,
        ));
        sig
    }

    fn import_intrinsic(
        func: &mut ir::Function, intrinsic: intr::Intrinsic, sig: ir::Signature,
    ) -> ir::FuncRef {
        let signature = func.import_signature(sig);
        let name =
            ir::ExternalName::User(func.declare_imported_user_function(ir::UserExternalName {
                namespace: intr::UserNamespace::Intrinsic.into(),
                index: intrinsic.into(),
            }));
        func.import_function(ir::ExtFuncData {
            name,
            signature,
            colocated: false,
        })
    }

    fn cached_intrinsic(
        &mut self, func: &mut ir::Function, intrinsic: intr::Intrinsic, sig: ir::Signature,
    ) -> ir::FuncRef {
        let key = u32::from(intrinsic);
        if let Some(fref) = self.intrinsics.get(&key).copied() {
            return fref;
        }

        let fref = Self::import_intrinsic(func, intrinsic, sig);
        self.intrinsics.insert(key, fref);
        fref
    }

    cached_intrinsic_method!(
        intrinsic_mem_grow,
        intr::Intrinsic::MemGrow,
        [I32, vmctx] -> [I32]
    );
    cached_intrinsic_method!(
        intrinsic_mem_size,
        intr::Intrinsic::MemSize,
        [vmctx] -> [I32]
    );
    cached_intrinsic_method!(
        intrinsic_mem_copy,
        intr::Intrinsic::MemCopy,
        [I32, I32, I32, vmctx] -> []
    );
    cached_intrinsic_method!(
        intrinsic_mem_fill,
        intr::Intrinsic::MemFill,
        [I32, I32, I32, vmctx] -> []
    );
    cached_intrinsic_method!(
        intrinsic_mem_init,
        intr::Intrinsic::MemInit,
        [I32, I32, I32, I32, vmctx] -> []
    );
    cached_intrinsic_method!(
        intrinsic_data_drop,
        intr::Intrinsic::DataDrop,
        [I32, vmctx] -> []
    );
    cached_intrinsic_method!(
        intrinsic_table_copy,
        intr::Intrinsic::TableCopy,
        [I32, I32, I32, I32, I32, vmctx] -> []
    );
    cached_intrinsic_method!(
        intrinsic_table_init,
        intr::Intrinsic::TableInit,
        [I32, I32, I32, I32, I32, vmctx] -> []
    );
    cached_intrinsic_method!(
        intrinsic_elem_drop,
        intr::Intrinsic::ElemDrop,
        [I32, vmctx] -> []
    );
    cached_intrinsic_method!(
        intrinsic_table_get,
        intr::Intrinsic::TableGet,
        [I32, I32, vmctx] -> [I32]
    );
    cached_intrinsic_method!(
        intrinsic_table_set,
        intr::Intrinsic::TableSet,
        [I32, I32, I32, vmctx] -> []
    );
    cached_intrinsic_method!(
        intrinsic_table_fill,
        intr::Intrinsic::TableFill,
        [I32, I32, I32, I32, vmctx] -> []
    );
    cached_intrinsic_method!(
        intrinsic_table_size,
        intr::Intrinsic::TableSize,
        [I32, vmctx] -> [I32]
    );
    cached_intrinsic_method!(
        intrinsic_table_grow,
        intr::Intrinsic::TableGrow,
        [I32, I32, I32, vmctx] -> [I32]
    );

    cached_intrinsic_method!(
        intrinsic_global_get_i32,
        intr::Intrinsic::GlobalGetI32,
        [I32, vmctx] -> [I32]
    );
    cached_intrinsic_method!(
        intrinsic_global_get_i64,
        intr::Intrinsic::GlobalGetI64,
        [I32, vmctx] -> [I64]
    );
    cached_intrinsic_method!(
        intrinsic_global_get_f32,
        intr::Intrinsic::GlobalGetF32,
        [I32, vmctx] -> [F32]
    );
    cached_intrinsic_method!(
        intrinsic_global_get_f64,
        intr::Intrinsic::GlobalGetF64,
        [I32, vmctx] -> [F64]
    );

    cached_intrinsic_method!(
        intrinsic_global_set_i32,
        intr::Intrinsic::GlobalSetI32,
        [I32, I32, vmctx] -> []
    );
    cached_intrinsic_method!(
        intrinsic_global_set_i64,
        intr::Intrinsic::GlobalSetI64,
        [I32, I64, vmctx] -> []
    );
    cached_intrinsic_method!(
        intrinsic_global_set_f32,
        intr::Intrinsic::GlobalSetF32,
        [I32, F32, vmctx] -> []
    );
    cached_intrinsic_method!(
        intrinsic_global_set_f64,
        intr::Intrinsic::GlobalSetF64,
        [I32, F64, vmctx] -> []
    );

    fn intrinsic_ref_func(func: &mut ir::Function, func_index: FuncIndex) -> ir::FuncRef {
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.returns.push(ir::AbiParam::new(I32));
        Self::import_intrinsic(func, intr::Intrinsic::ref_func(func_index), sig)
    }

    fn intrinsic_global_get(&mut self, func: &mut ir::Function, ty: ir::Type) -> ir::FuncRef {
        match ty {
            I32 => self.intrinsic_global_get_i32(func),
            I64 => self.intrinsic_global_get_i64(func),
            F32 => self.intrinsic_global_get_f32(func),
            F64 => self.intrinsic_global_get_f64(func),
            _ => unreachable!("unsupported global type {ty}"),
        }
    }

    fn intrinsic_global_set(&mut self, func: &mut ir::Function, ty: ir::Type) -> ir::FuncRef {
        match ty {
            I32 => self.intrinsic_global_set_i32(func),
            I64 => self.intrinsic_global_set_i64(func),
            F32 => self.intrinsic_global_set_f32(func),
            F64 => self.intrinsic_global_set_f64(func),
            _ => unreachable!("unsupported global type {ty}"),
        }
    }

    fn intrinsic_call_indirect(
        &self, func: &mut ir::Function, table_index: TableIndex, sig_index: TypeIndex,
    ) -> ir::FuncRef {
        let mut sig = self.vmctx_sig(sig_index);
        sig.params.insert(0, ir::AbiParam::new(I32));
        let canonical_sig_index = self.mod_info.canonical_type_index(sig_index);
        Self::import_intrinsic(
            func,
            intr::Intrinsic::call_indirect(table_index, canonical_sig_index),
            sig,
        )
    }
}

impl TypeConvert for NixFuncEnvironment<'_> {
    fn lookup_heap_type(&self, _index: cranelift_wasm::wasmparser::UnpackedIndex) -> WasmHeapType {
        unimplemented!("GC/reference types not supported in wasm2nix v0")
    }

    fn lookup_type_index(
        &self, _index: cranelift_wasm::wasmparser::UnpackedIndex,
    ) -> cranelift_wasm::EngineOrModuleTypeIndex {
        unimplemented!("GC/reference types not supported in wasm2nix v0")
    }
}

impl TargetEnvironment for NixFuncEnvironment<'_> {
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
        (I32, false)
    }
}

impl FuncEnvironment for NixFuncEnvironment<'_> {
    fn param_needs_stack_map(&self, _signature: &ir::Signature, _index: usize) -> bool {
        false
    }

    fn sig_ref_result_needs_stack_map(&self, _sig_ref: ir::SigRef, _index: usize) -> bool {
        false
    }

    fn func_ref_result_needs_stack_map(
        &self, _func: &ir::Function, _func_ref: ir::FuncRef, _index: usize,
    ) -> bool {
        false
    }

    fn make_global(
        &mut self, _func: &mut ir::Function, _index: GlobalIndex,
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

        // Make the base address symbolic so `heap_addr` results are stable and
        // can be treated as linear offsets during CLIF->Nix lowering
        // (we'll interpret the base as 0).
        let base_name = func.declare_imported_user_function(ir::UserExternalName {
            namespace: intr::UserNamespace::Symbol.into(),
            index: 1000 + index.as_u32(),
        });
        let base = func.create_global_value(ir::GlobalValueData::Symbol {
            name: ir::ExternalName::User(base_name),
            offset: Imm64::new(0),
            colocated: false,
            tls: false,
        });

        // Allow addressing up to the wasm32 maximum (4GiB); runtime will
        // enforce actual size. The 2 GiB guard region, as in wasmtime, lets
        // Cranelift skip its own bounds checks for offsets below it: the
        // runtime bounds-checks every access anyway.
        let bound = WASM32_MAX_SIZE;

        Ok(self.heaps.push(HeapData {
            base,
            min_size: 0,
            max_size: Some(bound),
            offset_guard_size: 0x8000_0000,
            style: HeapStyle::Static { bound },
            index_type: I32,
            memory_type: None,
            page_size_log2: memory.page_size_log2,
        }))
    }

    fn make_indirect_sig(
        &mut self, func: &mut ir::Function, index: TypeIndex,
    ) -> WasmResult<ir::SigRef> {
        Ok(func.import_signature(self.vmctx_sig(index)))
    }

    fn make_direct_func(
        &mut self, func: &mut ir::Function, index: FuncIndex,
    ) -> WasmResult<ir::FuncRef> {
        let sigidx = self.mod_info.functions[index].entity;
        let signature = func.import_signature(self.vmctx_sig(sigidx));
        let name =
            ir::ExternalName::User(func.declare_imported_user_function(ir::UserExternalName {
                namespace: intr::UserNamespace::Func.into(),
                index: index.as_u32(),
            }));
        Ok(func.import_function(ir::ExtFuncData {
            name,
            signature,
            colocated: false,
        }))
    }

    fn translate_call(
        &mut self, builder: &mut FunctionBuilder, _callee_index: FuncIndex, callee: ir::FuncRef,
        call_args: &[ir::Value],
    ) -> WasmResult<ir::Inst> {
        let args = args_with_vmctx(builder.func, call_args.iter().copied())?;
        Ok(builder.ins().call(callee, &args))
    }

    fn translate_call_indirect(
        &mut self, builder: &mut FunctionBuilder, table_index: TableIndex, sig_index: TypeIndex,
        _sig_ref: ir::SigRef, callee: ir::Value, call_args: &[ir::Value],
    ) -> WasmResult<Option<ir::Inst>> {
        let fref = self.intrinsic_call_indirect(builder.func, table_index, sig_index);
        let args = args_with_vmctx(
            builder.func,
            std::iter::once(callee).chain(call_args.iter().copied()),
        )?;
        Ok(Some(builder.ins().call(fref, &args)))
    }

    fn translate_return_call_indirect(
        &mut self, _builder: &mut FunctionBuilder, _table_index: TableIndex, _sig_index: TypeIndex,
        _sig_ref: ir::SigRef, _callee: ir::Value, _call_args: &[ir::Value],
    ) -> WasmResult<()> {
        Err(WasmError::Unsupported("return_call_indirect".into()))
    }

    fn translate_return_call_ref(
        &mut self, _builder: &mut FunctionBuilder, _sig_ref: ir::SigRef, _callee: ir::Value,
        _call_args: &[ir::Value],
    ) -> WasmResult<()> {
        Err(WasmError::Unsupported("return_call_ref".into()))
    }

    fn translate_call_ref(
        &mut self, _builder: &mut FunctionBuilder, _sig_ref: ir::SigRef, _callee: ir::Value,
        _call_args: &[ir::Value],
    ) -> WasmResult<ir::Inst> {
        Err(WasmError::Unsupported("call_ref".into()))
    }

    fn translate_memory_grow(
        &mut self, mut pos: FuncCursor, index: MemoryIndex, _heap: Heap, val: ir::Value,
    ) -> WasmResult<ir::Value> {
        ensure_single_memory(index)?;
        let fref = self.intrinsic_mem_grow(pos.func);
        call_cursor_result(&mut pos, fref, [val])
    }

    fn translate_memory_size(
        &mut self, mut pos: FuncCursor, index: MemoryIndex, _heap: Heap,
    ) -> WasmResult<ir::Value> {
        ensure_single_memory(index)?;
        let fref = self.intrinsic_mem_size(pos.func);
        call_cursor_result(&mut pos, fref, [])
    }

    fn translate_memory_copy(
        &mut self, mut pos: FuncCursor, src_index: MemoryIndex, _src_heap: Heap,
        dst_index: MemoryIndex, _dst_heap: Heap, dst: ir::Value, src: ir::Value, len: ir::Value,
    ) -> WasmResult<()> {
        ensure_single_memory(src_index)?;
        ensure_single_memory(dst_index)?;
        let fref = self.intrinsic_mem_copy(pos.func);
        call_cursor(&mut pos, fref, [dst, src, len])?;
        Ok(())
    }

    fn translate_memory_fill(
        &mut self, mut pos: FuncCursor, index: MemoryIndex, _heap: Heap, dst: ir::Value,
        val: ir::Value, len: ir::Value,
    ) -> WasmResult<()> {
        ensure_single_memory(index)?;
        let fref = self.intrinsic_mem_fill(pos.func);
        call_cursor(&mut pos, fref, [dst, val, len])?;
        Ok(())
    }

    fn translate_memory_init(
        &mut self, mut pos: FuncCursor, index: MemoryIndex, _heap: Heap, seg_index: u32,
        dst: ir::Value, src: ir::Value, len: ir::Value,
    ) -> WasmResult<()> {
        ensure_single_memory(index)?;
        let seg = cursor_u32_const(&mut pos, seg_index);
        let fref = self.intrinsic_mem_init(pos.func);
        call_cursor(&mut pos, fref, [seg, dst, src, len])?;
        Ok(())
    }

    fn translate_data_drop(&mut self, mut pos: FuncCursor, seg_index: u32) -> WasmResult<()> {
        let seg = cursor_u32_const(&mut pos, seg_index);
        let fref = self.intrinsic_data_drop(pos.func);
        call_cursor(&mut pos, fref, [seg])?;
        Ok(())
    }

    fn translate_table_size(
        &mut self, mut pos: FuncCursor, index: TableIndex,
    ) -> WasmResult<ir::Value> {
        let table = cursor_u32_const(&mut pos, index.as_u32());
        let fref = self.intrinsic_table_size(pos.func);
        call_cursor_result(&mut pos, fref, [table])
    }

    fn translate_table_grow(
        &mut self, mut pos: FuncCursor, table_index: TableIndex, delta: ir::Value,
        init_value: ir::Value,
    ) -> WasmResult<ir::Value> {
        let table = cursor_u32_const(&mut pos, table_index.as_u32());
        let fref = self.intrinsic_table_grow(pos.func);
        call_cursor_result(&mut pos, fref, [table, init_value, delta])
    }

    fn translate_table_get(
        &mut self, builder: &mut FunctionBuilder, table_index: TableIndex, index: ir::Value,
    ) -> WasmResult<ir::Value> {
        let table = builder_u32_const(builder, table_index.as_u32());
        let fref = self.intrinsic_table_get(builder.func);
        call_builder_result(builder, fref, [table, index])
    }

    fn translate_table_set(
        &mut self, builder: &mut FunctionBuilder, table_index: TableIndex, value: ir::Value,
        index: ir::Value,
    ) -> WasmResult<()> {
        let table = builder_u32_const(builder, table_index.as_u32());
        let fref = self.intrinsic_table_set(builder.func);
        call_builder(builder, fref, [table, index, value])?;
        Ok(())
    }

    fn translate_table_copy(
        &mut self, mut pos: FuncCursor, dst_table_index: TableIndex, src_table_index: TableIndex,
        dst: ir::Value, src: ir::Value, len: ir::Value,
    ) -> WasmResult<()> {
        let dst_table = cursor_u32_const(&mut pos, dst_table_index.as_u32());
        let src_table = cursor_u32_const(&mut pos, src_table_index.as_u32());
        let fref = self.intrinsic_table_copy(pos.func);
        call_cursor(&mut pos, fref, [dst_table, src_table, dst, src, len])?;
        Ok(())
    }

    fn translate_table_fill(
        &mut self, mut pos: FuncCursor, table_index: TableIndex, dst: ir::Value, val: ir::Value,
        len: ir::Value,
    ) -> WasmResult<()> {
        let table = cursor_u32_const(&mut pos, table_index.as_u32());
        let fref = self.intrinsic_table_fill(pos.func);
        call_cursor(&mut pos, fref, [table, dst, val, len])?;
        Ok(())
    }

    fn translate_table_init(
        &mut self, mut pos: FuncCursor, seg_index: u32, table_index: TableIndex, dst: ir::Value,
        src: ir::Value, len: ir::Value,
    ) -> WasmResult<()> {
        let table = cursor_u32_const(&mut pos, table_index.as_u32());
        let seg = cursor_u32_const(&mut pos, seg_index);
        let fref = self.intrinsic_table_init(pos.func);
        call_cursor(&mut pos, fref, [table, seg, dst, src, len])?;
        Ok(())
    }

    fn translate_elem_drop(&mut self, mut pos: FuncCursor, seg_index: u32) -> WasmResult<()> {
        let seg = cursor_u32_const(&mut pos, seg_index);
        let fref = self.intrinsic_elem_drop(pos.func);
        call_cursor(&mut pos, fref, [seg])?;
        Ok(())
    }

    fn translate_ref_null(
        &mut self, mut pos: FuncCursor, _ty: WasmHeapType,
    ) -> WasmResult<ir::Value> {
        Ok(pos.ins().iconst(I32, 0))
    }

    fn translate_ref_is_null(
        &mut self, mut pos: FuncCursor, value: ir::Value,
    ) -> WasmResult<ir::Value> {
        let is_null = pos.ins().icmp_imm(IntCC::Equal, value, 0);
        Ok(pos.ins().uextend(I32, is_null))
    }

    fn translate_ref_func(
        &mut self, mut pos: FuncCursor, func_index: FuncIndex,
    ) -> WasmResult<ir::Value> {
        let fref = Self::intrinsic_ref_func(pos.func, func_index);
        let call = pos.ins().call(fref, &[]);
        Ok(pos.func.dfg.inst_results(call)[0])
    }

    fn translate_custom_global_get(
        &mut self, builder: &mut FunctionBuilder, global_index: GlobalIndex,
    ) -> WasmResult<ir::Value> {
        let global = self.mod_info.globals[global_index].entity;
        let clif_ty = clif_type(global.wasm_ty)?;
        let idx = builder_u32_const(builder, global_index.as_u32());
        let fref = self.intrinsic_global_get(builder.func, clif_ty);
        call_builder_result(builder, fref, [idx])
    }

    fn translate_custom_global_set(
        &mut self, builder: &mut FunctionBuilder, global_index: GlobalIndex, val: ir::Value,
    ) -> WasmResult<()> {
        let global = self.mod_info.globals[global_index].entity;
        let clif_ty = clif_type(global.wasm_ty)?;
        let idx = builder_u32_const(builder, global_index.as_u32());
        let fref = self.intrinsic_global_set(builder.func, clif_ty);
        call_builder(builder, fref, [idx, val])?;
        Ok(())
    }

    fn translate_atomic_wait(
        &mut self, _pos: FuncCursor, _index: MemoryIndex, _heap: Heap, _addr: ir::Value,
        _expected: ir::Value, _timeout: ir::Value,
    ) -> WasmResult<ir::Value> {
        Err(WasmError::Unsupported("atomics/threads".into()))
    }

    fn translate_atomic_notify(
        &mut self, _pos: FuncCursor, _index: MemoryIndex, _heap: Heap, _addr: ir::Value,
        _count: ir::Value,
    ) -> WasmResult<ir::Value> {
        Err(WasmError::Unsupported("atomics/threads".into()))
    }

    fn translate_ref_i31(&mut self, _pos: FuncCursor, _val: ir::Value) -> WasmResult<ir::Value> {
        Err(WasmError::Unsupported("ref.i31".into()))
    }

    fn translate_i31_get_s(
        &mut self, _pos: FuncCursor, _i31ref: ir::Value,
    ) -> WasmResult<ir::Value> {
        Err(WasmError::Unsupported("i31.get_s".into()))
    }

    fn translate_i31_get_u(
        &mut self, _pos: FuncCursor, _i31ref: ir::Value,
    ) -> WasmResult<ir::Value> {
        Err(WasmError::Unsupported("i31.get_u".into()))
    }
}

fn ensure_single_memory(index: MemoryIndex) -> WasmResult<()> {
    if index.index() == 0 {
        Ok(())
    } else {
        Err(WasmError::Unsupported("multiple memories".into()))
    }
}

fn args_with_vmctx(
    func: &ir::Function, args: impl IntoIterator<Item = ir::Value>,
) -> WasmResult<Vec<ir::Value>> {
    let vmctx = vmctx_param(func)?;
    let iter = args.into_iter();
    let (lower_bound, upper_bound) = iter.size_hint();
    let mut args = Vec::with_capacity(upper_bound.unwrap_or(lower_bound) + 1);
    args.extend(iter);
    args.push(vmctx);
    Ok(args)
}

fn call_cursor<const N: usize>(
    pos: &mut FuncCursor<'_>, fref: ir::FuncRef, args: [ir::Value; N],
) -> WasmResult<ir::Inst> {
    let args = args_with_vmctx(pos.func, args)?;
    Ok(pos.ins().call(fref, &args))
}

fn call_cursor_result<const N: usize>(
    pos: &mut FuncCursor<'_>, fref: ir::FuncRef, args: [ir::Value; N],
) -> WasmResult<ir::Value> {
    let inst = call_cursor(pos, fref, args)?;
    Ok(pos.func.dfg.inst_results(inst)[0])
}

fn call_builder<const N: usize>(
    builder: &mut FunctionBuilder<'_>, fref: ir::FuncRef, args: [ir::Value; N],
) -> WasmResult<ir::Inst> {
    let args = args_with_vmctx(builder.func, args)?;
    Ok(builder.ins().call(fref, &args))
}

fn call_builder_result<const N: usize>(
    builder: &mut FunctionBuilder<'_>, fref: ir::FuncRef, args: [ir::Value; N],
) -> WasmResult<ir::Value> {
    let inst = call_builder(builder, fref, args)?;
    Ok(builder.inst_results(inst)[0])
}

fn cursor_u32_const(pos: &mut FuncCursor<'_>, value: u32) -> ir::Value {
    pos.ins().iconst(I32, i64::from(value))
}

fn builder_u32_const(builder: &mut FunctionBuilder<'_>, value: u32) -> ir::Value {
    builder.ins().iconst(I32, i64::from(value))
}

fn vmctx_param(func: &ir::Function) -> WasmResult<ir::Value> {
    func.special_param(ir::ArgumentPurpose::VMContext)
        .ok_or_else(|| WasmError::Unsupported("missing VMContext parameter".into()))
}
