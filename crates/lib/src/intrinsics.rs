use cranelift_wasm::{FuncIndex, TypeIndex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserNamespace {
    Func,
    Intrinsic,
    Symbol,
}

impl UserNamespace {
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Func => 0,
            Self::Intrinsic => 1,
            Self::Symbol => 2,
        }
    }

    pub const fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Func),
            1 => Some(Self::Intrinsic),
            2 => Some(Self::Symbol),
            _ => None,
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
    const CALL_INDIRECT_BASE: u32 = 1000;
    const CALL_INDIRECT_STRIDE: u32 = 10000;
    const REF_FUNC_BASE: u32 = 1_000_000_000;

    pub fn index(self) -> u32 {
        match self {
            Self::MemGrow => 0,
            Self::MemSize => 1,
            Self::MemCopy => 2,
            Self::MemFill => 3,
            Self::MemInit => 4,
            Self::DataDrop => 5,
            Self::GlobalGetI32 => 10,
            Self::GlobalGetI64 => 11,
            Self::GlobalGetF32 => 12,
            Self::GlobalGetF64 => 13,
            Self::GlobalSetI32 => 20,
            Self::GlobalSetI64 => 21,
            Self::GlobalSetF32 => 22,
            Self::GlobalSetF64 => 23,
            Self::TableCopy => 30,
            Self::TableInit => 31,
            Self::ElemDrop => 32,
            Self::TableGet => 33,
            Self::TableSet => 34,
            Self::TableFill => 35,
            Self::TableSize => 36,
            Self::TableGrow => 37,
            Self::CallIndirect {
                table_index,
                type_index,
            } => Self::CALL_INDIRECT_BASE + table_index * Self::CALL_INDIRECT_STRIDE + type_index,
            Self::RefFunc { func_index } => Self::REF_FUNC_BASE + func_index.as_u32(),
        }
    }

    pub fn from_index(index: u32) -> Option<Self> {
        match index {
            0 => Some(Self::MemGrow),
            1 => Some(Self::MemSize),
            2 => Some(Self::MemCopy),
            3 => Some(Self::MemFill),
            4 => Some(Self::MemInit),
            5 => Some(Self::DataDrop),
            10 => Some(Self::GlobalGetI32),
            11 => Some(Self::GlobalGetI64),
            12 => Some(Self::GlobalGetF32),
            13 => Some(Self::GlobalGetF64),
            20 => Some(Self::GlobalSetI32),
            21 => Some(Self::GlobalSetI64),
            22 => Some(Self::GlobalSetF32),
            23 => Some(Self::GlobalSetF64),
            30 => Some(Self::TableCopy),
            31 => Some(Self::TableInit),
            32 => Some(Self::ElemDrop),
            33 => Some(Self::TableGet),
            34 => Some(Self::TableSet),
            35 => Some(Self::TableFill),
            36 => Some(Self::TableSize),
            37 => Some(Self::TableGrow),
            other if other >= Self::REF_FUNC_BASE => Some(Self::RefFunc {
                func_index: FuncIndex::from_u32(other - Self::REF_FUNC_BASE),
            }),
            other if other >= Self::CALL_INDIRECT_BASE => {
                let encoded = other - Self::CALL_INDIRECT_BASE;
                Some(Self::CallIndirect {
                    table_index: encoded / Self::CALL_INDIRECT_STRIDE,
                    type_index: encoded % Self::CALL_INDIRECT_STRIDE,
                })
            }
            _ => None,
        }
    }

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
