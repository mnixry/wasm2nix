use cranelift_wasm::{FuncIndex, TypeIndex};
use snafu::Snafu;

#[derive(Debug, Snafu)]
pub enum IntrinsicCastError {
    #[snafu(display("user namespace cast failed, unexpected value: {value}"))]
    UserNamespaceCast { value: u32 },
    #[snafu(display("intrinsic index cast failed, unexpected value: {value}"))]
    IntrinsicIndexCast { value: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserNamespace {
    Func,
    Intrinsic,
    Symbol,
}

impl TryFrom<u32> for UserNamespace {
    type Error = IntrinsicCastError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Func),
            1 => Ok(Self::Intrinsic),
            2 => Ok(Self::Symbol),
            _ => IntrinsicIndexCastSnafu { value }.fail(),
        }
    }
}

impl From<UserNamespace> for u32 {
    fn from(val: UserNamespace) -> Self {
        match val {
            UserNamespace::Func => 0,
            UserNamespace::Intrinsic => 1,
            UserNamespace::Symbol => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intrinsic {
    MemGrow,
    MemSize,
    MemCopy,
    MemFill,
    MemInit,
    DataDrop,
    GlobalGetI32,
    GlobalGetI64,
    GlobalGetF32,
    GlobalGetF64,
    GlobalSetI32,
    GlobalSetI64,
    GlobalSetF32,
    GlobalSetF64,
    TableCopy,
    TableInit,
    ElemDrop,
    TableGet,
    TableSet,
    TableFill,
    TableSize,
    TableGrow,
    CallIndirect { table_index: u32, type_index: u32 },
    RefFunc { func_index: FuncIndex },
}

impl Intrinsic {
    pub const CALL_INDIRECT_BASE: u32 = 1000;
    pub const CALL_INDIRECT_STRIDE: u32 = 10000;
    pub const REF_FUNC_BASE: u32 = 1_000_000_000;

    pub fn call_indirect(table_index: cranelift_wasm::TableIndex, type_index: TypeIndex) -> Self {
        Self::CallIndirect {
            table_index: table_index.as_u32(),
            type_index: type_index.as_u32(),
        }
    }

    pub fn ref_func(func_index: FuncIndex) -> Self {
        Self::RefFunc { func_index }
    }
}

impl From<Intrinsic> for u32 {
    fn from(val: Intrinsic) -> Self {
        match val {
            Intrinsic::MemGrow => 0,
            Intrinsic::MemSize => 1,
            Intrinsic::MemCopy => 2,
            Intrinsic::MemFill => 3,
            Intrinsic::MemInit => 4,
            Intrinsic::DataDrop => 5,
            Intrinsic::GlobalGetI32 => 10,
            Intrinsic::GlobalGetI64 => 11,
            Intrinsic::GlobalGetF32 => 12,
            Intrinsic::GlobalGetF64 => 13,
            Intrinsic::GlobalSetI32 => 20,
            Intrinsic::GlobalSetI64 => 21,
            Intrinsic::GlobalSetF32 => 22,
            Intrinsic::GlobalSetF64 => 23,
            Intrinsic::TableCopy => 30,
            Intrinsic::TableInit => 31,
            Intrinsic::ElemDrop => 32,
            Intrinsic::TableGet => 33,
            Intrinsic::TableSet => 34,
            Intrinsic::TableFill => 35,
            Intrinsic::TableSize => 36,
            Intrinsic::TableGrow => 37,
            Intrinsic::CallIndirect {
                table_index,
                type_index,
            } => {
                Intrinsic::CALL_INDIRECT_BASE
                    + table_index * Intrinsic::CALL_INDIRECT_STRIDE
                    + type_index
            }
            Intrinsic::RefFunc { func_index } => Intrinsic::REF_FUNC_BASE + func_index.as_u32(),
        }
    }
}

impl TryFrom<u32> for Intrinsic {
    type Error = IntrinsicCastError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::MemGrow),
            1 => Ok(Self::MemSize),
            2 => Ok(Self::MemCopy),
            3 => Ok(Self::MemFill),
            4 => Ok(Self::MemInit),
            5 => Ok(Self::DataDrop),
            10 => Ok(Self::GlobalGetI32),
            11 => Ok(Self::GlobalGetI64),
            12 => Ok(Self::GlobalGetF32),
            13 => Ok(Self::GlobalGetF64),
            20 => Ok(Self::GlobalSetI32),
            21 => Ok(Self::GlobalSetI64),
            22 => Ok(Self::GlobalSetF32),
            23 => Ok(Self::GlobalSetF64),
            30 => Ok(Self::TableCopy),
            31 => Ok(Self::TableInit),
            32 => Ok(Self::ElemDrop),
            33 => Ok(Self::TableGet),
            34 => Ok(Self::TableSet),
            35 => Ok(Self::TableFill),
            36 => Ok(Self::TableSize),
            37 => Ok(Self::TableGrow),
            other if other >= Self::REF_FUNC_BASE => Ok(Self::RefFunc {
                func_index: FuncIndex::from_u32(other - Self::REF_FUNC_BASE),
            }),
            other if other >= Self::CALL_INDIRECT_BASE => {
                let encoded = other - Self::CALL_INDIRECT_BASE;
                Ok(Self::CallIndirect {
                    table_index: encoded / Self::CALL_INDIRECT_STRIDE,
                    type_index: encoded % Self::CALL_INDIRECT_STRIDE,
                })
            }
            _ => IntrinsicIndexCastSnafu { value }.fail(),
        }
    }
}
