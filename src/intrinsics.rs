pub const FUNC_NAMESPACE: u32 = 0;
pub const INTRINSIC_NAMESPACE: u32 = 1;

pub const MEM_GROW: u32 = 0;
pub const MEM_SIZE: u32 = 1;
pub const MEM_COPY: u32 = 2;
pub const MEM_FILL: u32 = 3;
pub const MEM_INIT: u32 = 4;
pub const DATA_DROP: u32 = 5;

pub const GLOBAL_GET_I32: u32 = 10;
pub const GLOBAL_GET_I64: u32 = 11;
pub const GLOBAL_GET_F32: u32 = 12;
pub const GLOBAL_GET_F64: u32 = 13;
pub const GLOBAL_SET_I32: u32 = 20;
pub const GLOBAL_SET_I64: u32 = 21;
pub const GLOBAL_SET_F32: u32 = 22;
pub const GLOBAL_SET_F64: u32 = 23;

pub const TABLE_COPY: u32 = 30;
pub const TABLE_INIT: u32 = 31;
pub const ELEM_DROP: u32 = 32;
pub const TABLE_GET: u32 = 33;
pub const TABLE_SET: u32 = 34;
pub const TABLE_FILL: u32 = 35;
pub const TABLE_SIZE: u32 = 36;
pub const TABLE_GROW: u32 = 37;

pub const CALL_INDIRECT_BASE: u32 = 1000;
pub const CALL_INDIRECT_STRIDE: u32 = 10000;
pub const REF_FUNC_BASE: u32 = 1_000_000_000;
