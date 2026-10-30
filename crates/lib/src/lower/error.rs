use cranelift_codegen::ir;
use cranelift_codegen::ir::Opcode;
use cranelift_wasm::ConstOp;
use cranelift_wasm::GlobalIndex;
use cranelift_wasm::TypeIndex;
use cranelift_wasm::WasmValType;
use snafu::Snafu;
use wasm2nix_runtime::intrinsics::UserNameError;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum LowerError {
    #[snafu(display("function f{func_index} has no entry block"))]
    MissingEntryBlock { func_index: u32 },

    #[snafu(display("function f{func_index} has no VMContext parameter"))]
    MissingVmContext { func_index: u32 },

    #[snafu(display("block b{block} has no terminator"))]
    EmptyBlock { block: u32 },

    #[snafu(display("unsupported global type {ty}"))]
    UnsupportedGlobalType { ty: WasmValType },

    #[snafu(display("unsupported memory access {opcode} for type {ty}"))]
    UnsupportedMemoryAccess { opcode: Opcode, ty: ir::Type },

    #[snafu(display("unsupported opcode {opcode}"))]
    UnsupportedOpcode { opcode: Opcode },

    #[snafu(display("unsupported external name {name}"))]
    UnsupportedExternalName { name: String },

    #[snafu(display("invalid external function name"))]
    InvalidUserName { source: UserNameError },

    #[snafu(display("global {index:?} has no initializer"))]
    MissingGlobalInitializer { index: GlobalIndex },

    #[snafu(display("missing type {type_index:?}"))]
    MissingType { type_index: TypeIndex },

    #[snafu(display("const expression operator {op:?} is missing an operand"))]
    ConstExprOperand { op: ConstOp },

    #[snafu(display("unsupported const expression operator {op:?}"))]
    UnsupportedConstOp { op: ConstOp },

    #[snafu(display("const expression produces {count} values instead of one"))]
    ConstExprResults { count: usize },
}

pub type Result<T> = std::result::Result<T, LowerError>;
