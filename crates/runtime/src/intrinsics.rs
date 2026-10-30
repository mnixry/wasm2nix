//! The functions and symbols the translator declares in Cranelift IR, and
//! their `UserExternalName { namespace, index }` encoding. Every kind has its
//! own namespace, so the encoding is lossless.

use cranelift_wasm::FuncIndex;
use cranelift_wasm::MemoryIndex;
use cranelift_wasm::TableIndex;
use cranelift_wasm::TypeIndex;
use cranelift_wasm::WasmValType;
use snafu::OptionExt;
use snafu::Snafu;

use crate::RuntimeFunction;

#[derive(Debug, Snafu)]
pub enum UserNameError {
    #[snafu(display("unknown intrinsic code {code}"))]
    UnknownIntrinsic { code: u32 },

    #[snafu(display("table index {table_index:?} exceeds the call_indirect namespaces"))]
    TableIndexOverflow { table_index: TableIndex },
}

/// A Cranelift user external name declared by the translator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserName {
    /// A Wasm function.
    Func(FuncIndex),
    /// The symbolic base address of a linear memory, which lowers as 0.
    MemoryBase(MemoryIndex),
    Intrinsic(Intrinsic),
}

/// The raw parts of a [`UserName`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserNameParts {
    pub namespace: u32,
    pub index: u32,
}

const FUNC_NAMESPACE: u32 = 0;
const FIXED_INTRINSIC_NAMESPACE: u32 = 1;
const MEMORY_BASE_NAMESPACE: u32 = 2;
const REF_FUNC_NAMESPACE: u32 = 3;
/// `call_indirect` through table `t` uses namespace `CALL_INDIRECT_NAMESPACE +
/// t`.
const CALL_INDIRECT_NAMESPACE: u32 = 4;

impl UserName {
    /// # Errors
    ///
    /// Returns an error when a `call_indirect` table index does not fit the
    /// namespace range.
    pub fn to_parts(self) -> Result<UserNameParts, UserNameError> {
        let (namespace, index) = match self {
            Self::Func(func) => (FUNC_NAMESPACE, func.as_u32()),
            Self::MemoryBase(memory) => (MEMORY_BASE_NAMESPACE, memory.as_u32()),
            Self::Intrinsic(Intrinsic::Fixed(fixed)) => (FIXED_INTRINSIC_NAMESPACE, fixed as u32),
            Self::Intrinsic(Intrinsic::RefFunc { func_index }) => {
                (REF_FUNC_NAMESPACE, func_index.as_u32())
            }
            Self::Intrinsic(Intrinsic::CallIndirect {
                table_index,
                type_index,
            }) => (
                CALL_INDIRECT_NAMESPACE
                    .checked_add(table_index.as_u32())
                    .context(TableIndexOverflowSnafu { table_index })?,
                type_index.as_u32(),
            ),
        };
        Ok(UserNameParts { namespace, index })
    }

    /// # Errors
    ///
    /// Returns an error for a fixed intrinsic code that does not exist.
    pub fn from_parts(parts: UserNameParts) -> Result<Self, UserNameError> {
        let UserNameParts { namespace, index } = parts;
        if let Some(table) = namespace.checked_sub(CALL_INDIRECT_NAMESPACE) {
            return Ok(Self::Intrinsic(Intrinsic::CallIndirect {
                table_index: TableIndex::from_u32(table),
                type_index: TypeIndex::from_u32(index),
            }));
        }
        Ok(match namespace {
            FUNC_NAMESPACE => Self::Func(FuncIndex::from_u32(index)),
            MEMORY_BASE_NAMESPACE => Self::MemoryBase(MemoryIndex::from_u32(index)),
            FIXED_INTRINSIC_NAMESPACE => Self::Intrinsic(Intrinsic::Fixed(
                usize::try_from(index)
                    .ok()
                    .and_then(|code| FixedIntrinsic::ALL.get(code))
                    .copied()
                    .context(UnknownIntrinsicSnafu { code: index })?,
            )),
            // The only namespace below `CALL_INDIRECT_NAMESPACE` left.
            _ => Self::Intrinsic(Intrinsic::RefFunc {
                func_index: FuncIndex::from_u32(index),
            }),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Intrinsic {
    /// A runtime function with a fixed signature.
    Fixed(FixedIntrinsic),
    CallIndirect {
        table_index: TableIndex,
        type_index: TypeIndex,
    },
    RefFunc {
        func_index: FuncIndex,
    },
}

/// The intrinsics without operands. The discriminant is the encoded code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FixedIntrinsic {
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
}

impl FixedIntrinsic {
    /// Every variant, in discriminant order.
    const ALL: [Self; 22] = [
        Self::MemGrow,
        Self::MemSize,
        Self::MemCopy,
        Self::MemFill,
        Self::MemInit,
        Self::DataDrop,
        Self::GlobalGetI32,
        Self::GlobalGetI64,
        Self::GlobalGetF32,
        Self::GlobalGetF64,
        Self::GlobalSetI32,
        Self::GlobalSetI64,
        Self::GlobalSetF32,
        Self::GlobalSetF64,
        Self::TableCopy,
        Self::TableInit,
        Self::ElemDrop,
        Self::TableGet,
        Self::TableSet,
        Self::TableFill,
        Self::TableSize,
        Self::TableGrow,
    ];

    /// The runtime function implementing this intrinsic.
    #[must_use]
    pub const fn runtime_function(self) -> RuntimeFunction {
        match self {
            Self::MemGrow => RuntimeFunction::MemGrow,
            Self::MemSize => RuntimeFunction::MemSize,
            Self::MemCopy => RuntimeFunction::MemCopy,
            Self::MemFill => RuntimeFunction::MemFill,
            Self::MemInit => RuntimeFunction::MemInit,
            Self::DataDrop => RuntimeFunction::MemDataDrop,
            Self::GlobalGetI32 => RuntimeFunction::GlobalGetI32,
            Self::GlobalGetI64 => RuntimeFunction::GlobalGetI64,
            Self::GlobalGetF32 => RuntimeFunction::GlobalGetF32,
            Self::GlobalGetF64 => RuntimeFunction::GlobalGetF64,
            Self::GlobalSetI32 => RuntimeFunction::GlobalSetI32,
            Self::GlobalSetI64 => RuntimeFunction::GlobalSetI64,
            Self::GlobalSetF32 => RuntimeFunction::GlobalSetF32,
            Self::GlobalSetF64 => RuntimeFunction::GlobalSetF64,
            Self::TableCopy => RuntimeFunction::TableCopy,
            Self::TableInit => RuntimeFunction::TableInit,
            Self::ElemDrop => RuntimeFunction::TableElemDrop,
            Self::TableGet => RuntimeFunction::TableGet,
            Self::TableSet => RuntimeFunction::TableSet,
            Self::TableFill => RuntimeFunction::TableFill,
            Self::TableSize => RuntimeFunction::TableSize,
            Self::TableGrow => RuntimeFunction::TableGrow,
        }
    }
}

/// The intrinsics reading and writing one global.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalAccessors {
    pub get: FixedIntrinsic,
    pub set: FixedIntrinsic,
}

/// The accessors of a global of type `ty`; references are i32 table entries.
/// `None` for `v128`, which the runtime cannot represent.
#[must_use]
pub const fn global_accessors(ty: &WasmValType) -> Option<GlobalAccessors> {
    let (get, set) = match ty {
        WasmValType::I32 | WasmValType::Ref(_) => {
            (FixedIntrinsic::GlobalGetI32, FixedIntrinsic::GlobalSetI32)
        }
        WasmValType::I64 => (FixedIntrinsic::GlobalGetI64, FixedIntrinsic::GlobalSetI64),
        WasmValType::F32 => (FixedIntrinsic::GlobalGetF32, FixedIntrinsic::GlobalSetF32),
        WasmValType::F64 => (FixedIntrinsic::GlobalGetF64, FixedIntrinsic::GlobalSetF64),
        WasmValType::V128 => return None,
    };
    Some(GlobalAccessors { get, set })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_intrinsic_table_follows_discriminants() {
        for (code, fixed) in (0..).zip(FixedIntrinsic::ALL) {
            assert_eq!(fixed as u32, code, "{fixed:?}");
        }
    }

    #[test]
    fn user_names_round_trip() {
        let names = FixedIntrinsic::ALL
            .into_iter()
            .map(|fixed| UserName::Intrinsic(Intrinsic::Fixed(fixed)))
            .chain([
                UserName::Func(FuncIndex::from_u32(7)),
                UserName::MemoryBase(MemoryIndex::from_u32(0)),
                UserName::Intrinsic(Intrinsic::RefFunc {
                    func_index: FuncIndex::from_u32(999_999),
                }),
                UserName::Intrinsic(Intrinsic::CallIndirect {
                    table_index: TableIndex::from_u32(99),
                    type_index: TypeIndex::from_u32(999_999),
                }),
            ]);
        for name in names {
            let parts = name.to_parts().unwrap();
            assert_eq!(UserName::from_parts(parts).unwrap(), name);
        }
    }

    #[test]
    fn rejects_unknown_codes_and_overflowing_tables() {
        let unknown = UserNameParts {
            namespace: FIXED_INTRINSIC_NAMESPACE,
            index: 22,
        };
        assert!(matches!(
            UserName::from_parts(unknown),
            Err(UserNameError::UnknownIntrinsic { code: 22 })
        ));
        let overflow = UserName::Intrinsic(Intrinsic::CallIndirect {
            table_index: TableIndex::from_u32(u32::MAX - 1),
            type_index: TypeIndex::from_u32(0),
        });
        assert!(matches!(
            overflow.to_parts(),
            Err(UserNameError::TableIndexOverflow { .. })
        ));
    }
}
