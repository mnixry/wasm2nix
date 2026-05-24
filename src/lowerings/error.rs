use snafu::prelude::*;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum LowerError {
    #[snafu(display("function f{func_index} has no entry block"))]
    MissingEntryBlock { func_index: u32 },

    #[snafu(display("function f{func_index} has no VMContext parameter"))]
    MissingVmContext { func_index: u32 },

    #[snafu(display("block b{block} has no terminator"))]
    EmptyBlock { block: u32 },

    #[snafu(display("unsupported Cranelift type {ty} while lowering {context}"))]
    UnsupportedClifType { context: &'static str, ty: String },

    #[snafu(display("unsupported global type {ty}"))]
    UnsupportedGlobalType { ty: String },

    #[snafu(display("unsupported load opcode {opcode} for type {ty}"))]
    UnsupportedLoad { opcode: String, ty: String },

    #[snafu(display("unsupported store opcode {opcode} for type {ty}"))]
    UnsupportedStore { opcode: String, ty: String },

    #[snafu(display("unsupported opcode {opcode}"))]
    UnsupportedOpcode { opcode: String },

    #[snafu(display("unsupported external name {name}"))]
    UnsupportedExternalName { name: String },

    #[snafu(display("unknown external name namespace {namespace}"))]
    UnknownExternalNamespace { namespace: u32 },

    #[snafu(display("unknown intrinsic index {index}"))]
    UnknownIntrinsic { index: u32 },

    #[snafu(display("missing function index {func_index}"))]
    MissingFunction { func_index: u32 },

    #[snafu(display("missing type index {type_index}"))]
    MissingType { type_index: u32 },

    #[snafu(display("malformed const expression: {reason}"))]
    MalformedConstExpr { reason: String },
}

pub type Result<T> = std::result::Result<T, LowerError>;
