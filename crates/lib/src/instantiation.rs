use std::convert::Infallible;

use cranelift_entity::EntityRef;
use cranelift_wasm::ConstExpr;
use cranelift_wasm::ConstOp;
use cranelift_wasm::GlobalIndex;
use cranelift_wasm::MemoryIndex;
use cranelift_wasm::TableIndex;
use snafu::OptionExt;
use snafu::ResultExt;
use snafu::Snafu;

use crate::translator::NixModule;
use crate::translator::WASM_PAGE_SIZE;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum InstantiationError<E: snafu::Error + 'static = Infallible> {
    #[snafu(display("failed to resolve active initializer input"))]
    Resolve { source: E },

    #[snafu(display("memory byte size overflow"))]
    MemoryByteSizeOverflow,

    #[snafu(display("global {index:?} has no constant initializer"))]
    MissingGlobalInitializer { index: GlobalIndex },

    #[snafu(display("imported i32 global has value {value}"))]
    GlobalValueOutOfRange { value: u64 },

    #[snafu(display("offset operator {op:?} is missing an operand"))]
    OffsetOperand { op: ConstOp },

    #[snafu(display("offset expression produces {count} values instead of one"))]
    OffsetResults { count: usize },

    #[snafu(display("unsupported active segment offset operator {op:?}"))]
    UnsupportedOffsetOp { op: ConstOp },
}

pub type Result<T, E = Infallible> = std::result::Result<T, InstantiationError<E>>;

pub trait ActiveInitializerResolver {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Returns the minimum page count for a memory.
    ///
    /// # Errors
    ///
    /// Returns an error when the memory cannot be resolved.
    fn memory_min_pages(
        &mut self, module: &NixModule, index: MemoryIndex,
    ) -> std::result::Result<u64, Self::Error>;

    /// Returns the minimum element count for a table.
    ///
    /// # Errors
    ///
    /// Returns an error when the table cannot be resolved.
    fn table_min_elements(
        &mut self, module: &NixModule, index: TableIndex,
    ) -> std::result::Result<u64, Self::Error>;

    /// Returns the current value of an imported i32 global.
    ///
    /// # Errors
    ///
    /// Returns an error when the imported global cannot be resolved.
    fn imported_global_i32(
        &mut self, module_name: &str, field_name: &str,
    ) -> std::result::Result<u64, Self::Error>;
}

/// Reports whether active data or element initializers would trap during
/// instantiation.
///
/// # Errors
///
/// Returns an error when an imported initializer input cannot be resolved, when
/// initializer arithmetic overflows, or when an initializer expression is
/// malformed or unsupported.
pub fn active_initializers_trap<R: ActiveInitializerResolver>(
    module: &NixModule, resolver: &mut R,
) -> Result<bool, R::Error> {
    for init in &module.info.data_initializers {
        let min_pages = resolver
            .memory_min_pages(module, init.memory_index)
            .context(ResolveSnafu)?;
        let byte_len = min_pages
            .checked_mul(WASM_PAGE_SIZE)
            .context(MemoryByteSizeOverflowSnafu)?;
        let offset = active_offset(module, resolver, init.base, init.offset)?;
        if offset.saturating_add(init.data.len() as u64) > byte_len {
            return Ok(true);
        }
    }

    for init in &module.info.table_initializers {
        let min_elements = resolver
            .table_min_elements(module, init.table_index)
            .context(ResolveSnafu)?;
        let offset = active_offset(module, resolver, init.base, u64::from(init.offset))?;
        if offset.saturating_add(init.elements.len() as u64) > min_elements {
            return Ok(true);
        }
    }

    Ok(false)
}

fn active_offset<R: ActiveInitializerResolver>(
    module: &NixModule, resolver: &mut R, base: Option<GlobalIndex>, offset: u64,
) -> Result<u64, R::Error> {
    let Some(base) = base else {
        return Ok(offset);
    };
    Ok(u64::from(global_i32_value(module, resolver, base)?).saturating_add(offset))
}

fn global_i32_value<R: ActiveInitializerResolver>(
    module: &NixModule, resolver: &mut R, index: GlobalIndex,
) -> Result<u32, R::Error> {
    if let Some((module_name, field_name)) = module.info.imported_globals.get(index.index()) {
        let value = resolver
            .imported_global_i32(module_name, field_name)
            .context(ResolveSnafu)?;
        return u32::try_from(value)
            .ok()
            .context(GlobalValueOutOfRangeSnafu { value });
    }

    let expr = module.info.global_inits[index]
        .as_ref()
        .context(MissingGlobalInitializerSnafu { index })?;
    eval_i32_const_expr(module, resolver, expr)
}

fn eval_i32_const_expr<R: ActiveInitializerResolver>(
    module: &NixModule, resolver: &mut R, expr: &ConstExpr,
) -> Result<u32, R::Error> {
    let mut stack = Vec::new();
    for op in expr.ops().iter().copied() {
        match op {
            ConstOp::I32Const(value) => stack.push(value.cast_unsigned()),
            ConstOp::GlobalGet(index) => stack.push(global_i32_value(module, resolver, index)?),
            ConstOp::I32Add | ConstOp::I32Sub | ConstOp::I32Mul => {
                let (rhs, lhs) = stack
                    .pop()
                    .zip(stack.pop())
                    .context(OffsetOperandSnafu { op })?;
                stack.push(match op {
                    ConstOp::I32Add => lhs.wrapping_add(rhs),
                    ConstOp::I32Sub => lhs.wrapping_sub(rhs),
                    _ => lhs.wrapping_mul(rhs),
                });
            }
            _ => return UnsupportedOffsetOpSnafu { op }.fail(),
        }
    }

    let count = stack.len();
    let [value] = <[u32; 1]>::try_from(stack)
        .ok()
        .context(OffsetResultsSnafu { count })?;
    Ok(value)
}
