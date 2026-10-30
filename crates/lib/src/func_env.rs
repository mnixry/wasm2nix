use std::collections::BTreeMap;

use cranelift_codegen::cursor::FuncCursor;
use cranelift_codegen::ir;
use cranelift_codegen::ir::InstBuilder;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::immediates::Imm64;
use cranelift_codegen::ir::types::F32;
use cranelift_codegen::ir::types::F64;
use cranelift_codegen::ir::types::I32;
use cranelift_codegen::ir::types::I64;
use cranelift_codegen::isa::CallConv;
use cranelift_entity::EntityRef;
use cranelift_entity::PrimaryMap;
use cranelift_frontend::FunctionBuilder;
use cranelift_wasm::FuncEnvironment;
use cranelift_wasm::FuncIndex;
use cranelift_wasm::GlobalIndex;
use cranelift_wasm::GlobalVariable;
use cranelift_wasm::Heap;
use cranelift_wasm::HeapData;
use cranelift_wasm::HeapStyle;
use cranelift_wasm::MemoryIndex;
use cranelift_wasm::TableIndex;
use cranelift_wasm::TargetEnvironment;
use cranelift_wasm::TypeIndex;
use cranelift_wasm::WASM32_MAX_SIZE;
use cranelift_wasm::WasmError;
use cranelift_wasm::WasmHeapType;
use cranelift_wasm::WasmResult;
use wasm2nix_runtime::intrinsics::FixedIntrinsic;
use wasm2nix_runtime::intrinsics::GlobalAccessors;
use wasm2nix_runtime::intrinsics::Intrinsic;
use wasm2nix_runtime::intrinsics::UserName;
use wasm2nix_runtime::intrinsics::global_accessors;

use crate::translator::NixModuleInfo;
use crate::translator::impl_target_environment;

/// Parameter and result types of a fixed intrinsic, before its trailing
/// `VMContext` parameter.
const fn fixed_signature(intrinsic: FixedIntrinsic) -> (&'static [ir::Type], &'static [ir::Type]) {
    match intrinsic {
        FixedIntrinsic::MemGrow | FixedIntrinsic::TableSize | FixedIntrinsic::GlobalGetI32 => {
            (&[I32], &[I32])
        }
        FixedIntrinsic::MemSize => (&[], &[I32]),
        FixedIntrinsic::MemCopy | FixedIntrinsic::MemFill | FixedIntrinsic::TableSet => {
            (&[I32, I32, I32], &[])
        }
        FixedIntrinsic::MemInit | FixedIntrinsic::TableFill => (&[I32, I32, I32, I32], &[]),
        FixedIntrinsic::DataDrop | FixedIntrinsic::ElemDrop => (&[I32], &[]),
        FixedIntrinsic::TableCopy | FixedIntrinsic::TableInit => (&[I32, I32, I32, I32, I32], &[]),
        FixedIntrinsic::TableGet => (&[I32, I32], &[I32]),
        FixedIntrinsic::TableGrow => (&[I32, I32, I32], &[I32]),
        FixedIntrinsic::GlobalGetI64 => (&[I32], &[I64]),
        FixedIntrinsic::GlobalGetF32 => (&[I32], &[F32]),
        FixedIntrinsic::GlobalGetF64 => (&[I32], &[F64]),
        FixedIntrinsic::GlobalSetI32 => (&[I32, I32], &[]),
        FixedIntrinsic::GlobalSetI64 => (&[I32, I64], &[]),
        FixedIntrinsic::GlobalSetF32 => (&[I32, F32], &[]),
        FixedIntrinsic::GlobalSetF64 => (&[I32, F64], &[]),
    }
}

pub struct NixFuncEnvironment<'m> {
    mod_info: &'m NixModuleInfo,
    heaps: PrimaryMap<Heap, HeapData>,
    /// Fixed intrinsics already declared in the current function.
    intrinsics: BTreeMap<FixedIntrinsic, ir::FuncRef>,
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
        let mut sig = self.mod_info.signature(sigidx).clone();
        sig.params.push(self.vmctx_param());
        sig
    }

    fn vmctx_param(&self) -> ir::AbiParam {
        ir::AbiParam::special(self.pointer_type(), ir::ArgumentPurpose::VMContext)
    }

    /// Declares `intrinsic` in `func` once and returns its reference.
    fn fixed_intrinsic(
        &mut self, func: &mut ir::Function, intrinsic: FixedIntrinsic,
    ) -> WasmResult<ir::FuncRef> {
        if let Some(fref) = self.intrinsics.get(&intrinsic) {
            return Ok(*fref);
        }
        let (params, results) = fixed_signature(intrinsic);
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.params
            .extend(params.iter().copied().map(ir::AbiParam::new));
        sig.params.push(self.vmctx_param());
        sig.returns
            .extend(results.iter().copied().map(ir::AbiParam::new));
        let fref = import_function(func, UserName::Intrinsic(Intrinsic::Fixed(intrinsic)), sig)?;
        self.intrinsics.insert(intrinsic, fref);
        Ok(fref)
    }

    fn global_accessors(&self, index: GlobalIndex) -> WasmResult<GlobalAccessors> {
        let ty = self
            .mod_info
            .globals
            .get(index)
            .ok_or_else(|| WasmError::User(format!("undeclared global {index:?}")))?
            .entity
            .wasm_ty;
        global_accessors(&ty).ok_or_else(|| WasmError::Unsupported(format!("global type {ty}")))
    }
}

/// Imports the function `name` into `func`.
fn import_function(
    func: &mut ir::Function, name: UserName, sig: ir::Signature,
) -> WasmResult<ir::FuncRef> {
    let signature = func.import_signature(sig);
    let name = ir::ExternalName::User(func.declare_imported_user_function(user_name(name)?));
    Ok(func.import_function(ir::ExtFuncData {
        name,
        signature,
        colocated: false,
    }))
}

/// Encodes `name` for Cranelift. Cranelift's environment traits only carry a
/// [`WasmError`], so the encoding error is kept as its message.
pub fn user_name(name: UserName) -> WasmResult<ir::UserExternalName> {
    let parts = name
        .to_parts()
        .map_err(|err| WasmError::User(err.to_string()))?;
    Ok(ir::UserExternalName::new(parts.namespace, parts.index))
}

impl_target_environment!(NixFuncEnvironment<'_>, |env| env.mod_info.config);

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
        ensure_single_memory(index)?;
        // Declarations already passed `validate_memory_type`.
        let memory = self
            .mod_info
            .memories
            .get(index)
            .map(|e| e.entity)
            .ok_or_else(|| WasmError::Unsupported("memory referenced but not declared".into()))?;

        // Make the base address symbolic so `heap_addr` results are stable and
        // can be treated as linear offsets during CLIF->Nix lowering
        // (we'll interpret the base as 0).
        let base_name =
            func.declare_imported_user_function(user_name(UserName::MemoryBase(index))?);
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
        let sigidx = self
            .mod_info
            .functions
            .get(index)
            .ok_or_else(|| WasmError::User(format!("undeclared function {index:?}")))?
            .entity;
        import_function(func, UserName::Func(index), self.vmctx_sig(sigidx))
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
        let mut sig = self.vmctx_sig(sig_index);
        sig.params.insert(0, ir::AbiParam::new(I32));
        let call_indirect = Intrinsic::CallIndirect {
            table_index,
            type_index: self.mod_info.canonical_type_index(sig_index),
        };
        let fref = import_function(builder.func, UserName::Intrinsic(call_indirect), sig)?;
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
        let fref = self.fixed_intrinsic(pos.func, FixedIntrinsic::MemGrow)?;
        call_cursor_result(&mut pos, fref, [val])
    }

    fn translate_memory_size(
        &mut self, mut pos: FuncCursor, index: MemoryIndex, _heap: Heap,
    ) -> WasmResult<ir::Value> {
        ensure_single_memory(index)?;
        let fref = self.fixed_intrinsic(pos.func, FixedIntrinsic::MemSize)?;
        call_cursor_result(&mut pos, fref, [])
    }

    fn translate_memory_copy(
        &mut self, mut pos: FuncCursor, src_index: MemoryIndex, _src_heap: Heap,
        dst_index: MemoryIndex, _dst_heap: Heap, dst: ir::Value, src: ir::Value, len: ir::Value,
    ) -> WasmResult<()> {
        ensure_single_memory(src_index)?;
        ensure_single_memory(dst_index)?;
        let fref = self.fixed_intrinsic(pos.func, FixedIntrinsic::MemCopy)?;
        call_cursor(&mut pos, fref, [dst, src, len])?;
        Ok(())
    }

    fn translate_memory_fill(
        &mut self, mut pos: FuncCursor, index: MemoryIndex, _heap: Heap, dst: ir::Value,
        val: ir::Value, len: ir::Value,
    ) -> WasmResult<()> {
        ensure_single_memory(index)?;
        let fref = self.fixed_intrinsic(pos.func, FixedIntrinsic::MemFill)?;
        call_cursor(&mut pos, fref, [dst, val, len])?;
        Ok(())
    }

    fn translate_memory_init(
        &mut self, mut pos: FuncCursor, index: MemoryIndex, _heap: Heap, seg_index: u32,
        dst: ir::Value, src: ir::Value, len: ir::Value,
    ) -> WasmResult<()> {
        ensure_single_memory(index)?;
        let seg = cursor_u32_const(&mut pos, seg_index);
        let fref = self.fixed_intrinsic(pos.func, FixedIntrinsic::MemInit)?;
        call_cursor(&mut pos, fref, [seg, dst, src, len])?;
        Ok(())
    }

    fn translate_data_drop(&mut self, mut pos: FuncCursor, seg_index: u32) -> WasmResult<()> {
        let seg = cursor_u32_const(&mut pos, seg_index);
        let fref = self.fixed_intrinsic(pos.func, FixedIntrinsic::DataDrop)?;
        call_cursor(&mut pos, fref, [seg])?;
        Ok(())
    }

    fn translate_table_size(
        &mut self, mut pos: FuncCursor, index: TableIndex,
    ) -> WasmResult<ir::Value> {
        let table = cursor_u32_const(&mut pos, index.as_u32());
        let fref = self.fixed_intrinsic(pos.func, FixedIntrinsic::TableSize)?;
        call_cursor_result(&mut pos, fref, [table])
    }

    fn translate_table_grow(
        &mut self, mut pos: FuncCursor, table_index: TableIndex, delta: ir::Value,
        init_value: ir::Value,
    ) -> WasmResult<ir::Value> {
        let table = cursor_u32_const(&mut pos, table_index.as_u32());
        let fref = self.fixed_intrinsic(pos.func, FixedIntrinsic::TableGrow)?;
        call_cursor_result(&mut pos, fref, [table, init_value, delta])
    }

    fn translate_table_get(
        &mut self, builder: &mut FunctionBuilder, table_index: TableIndex, index: ir::Value,
    ) -> WasmResult<ir::Value> {
        let table = builder_u32_const(builder, table_index.as_u32());
        let fref = self.fixed_intrinsic(builder.func, FixedIntrinsic::TableGet)?;
        call_builder_result(builder, fref, [table, index])
    }

    fn translate_table_set(
        &mut self, builder: &mut FunctionBuilder, table_index: TableIndex, value: ir::Value,
        index: ir::Value,
    ) -> WasmResult<()> {
        let table = builder_u32_const(builder, table_index.as_u32());
        let fref = self.fixed_intrinsic(builder.func, FixedIntrinsic::TableSet)?;
        call_builder(builder, fref, [table, index, value])?;
        Ok(())
    }

    fn translate_table_copy(
        &mut self, mut pos: FuncCursor, dst_table_index: TableIndex, src_table_index: TableIndex,
        dst: ir::Value, src: ir::Value, len: ir::Value,
    ) -> WasmResult<()> {
        let dst_table = cursor_u32_const(&mut pos, dst_table_index.as_u32());
        let src_table = cursor_u32_const(&mut pos, src_table_index.as_u32());
        let fref = self.fixed_intrinsic(pos.func, FixedIntrinsic::TableCopy)?;
        call_cursor(&mut pos, fref, [dst_table, src_table, dst, src, len])?;
        Ok(())
    }

    fn translate_table_fill(
        &mut self, mut pos: FuncCursor, table_index: TableIndex, dst: ir::Value, val: ir::Value,
        len: ir::Value,
    ) -> WasmResult<()> {
        let table = cursor_u32_const(&mut pos, table_index.as_u32());
        let fref = self.fixed_intrinsic(pos.func, FixedIntrinsic::TableFill)?;
        call_cursor(&mut pos, fref, [table, dst, val, len])?;
        Ok(())
    }

    fn translate_table_init(
        &mut self, mut pos: FuncCursor, seg_index: u32, table_index: TableIndex, dst: ir::Value,
        src: ir::Value, len: ir::Value,
    ) -> WasmResult<()> {
        let table = cursor_u32_const(&mut pos, table_index.as_u32());
        let seg = cursor_u32_const(&mut pos, seg_index);
        let fref = self.fixed_intrinsic(pos.func, FixedIntrinsic::TableInit)?;
        call_cursor(&mut pos, fref, [table, seg, dst, src, len])?;
        Ok(())
    }

    fn translate_elem_drop(&mut self, mut pos: FuncCursor, seg_index: u32) -> WasmResult<()> {
        let seg = cursor_u32_const(&mut pos, seg_index);
        let fref = self.fixed_intrinsic(pos.func, FixedIntrinsic::ElemDrop)?;
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
        let mut sig = ir::Signature::new(CallConv::Fast);
        sig.returns.push(ir::AbiParam::new(I32));
        let ref_func = Intrinsic::RefFunc { func_index };
        let fref = import_function(pos.func, UserName::Intrinsic(ref_func), sig)?;
        let call = pos.ins().call(fref, &[]);
        Ok(pos.func.dfg.first_result(call))
    }

    fn translate_custom_global_get(
        &mut self, builder: &mut FunctionBuilder, global_index: GlobalIndex,
    ) -> WasmResult<ir::Value> {
        let get = self.global_accessors(global_index)?.get;
        let idx = builder_u32_const(builder, global_index.as_u32());
        let fref = self.fixed_intrinsic(builder.func, get)?;
        call_builder_result(builder, fref, [idx])
    }

    fn translate_custom_global_set(
        &mut self, builder: &mut FunctionBuilder, global_index: GlobalIndex, val: ir::Value,
    ) -> WasmResult<()> {
        let set = self.global_accessors(global_index)?.set;
        let idx = builder_u32_const(builder, global_index.as_u32());
        let fref = self.fixed_intrinsic(builder.func, set)?;
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
    Ok(args.into_iter().chain([vmctx]).collect())
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
    Ok(pos.func.dfg.first_result(inst))
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
    Ok(builder.func.dfg.first_result(inst))
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
