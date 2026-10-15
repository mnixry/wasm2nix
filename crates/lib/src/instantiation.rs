use std::convert::Infallible;

use cranelift_entity::EntityRef;
use cranelift_wasm::{ConstExpr, ConstOp, GlobalIndex, MemoryIndex, TableIndex};
use snafu::prelude::*;

use crate::translator::NixModule;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum InstantiationError<E: snafu::Error + 'static = Infallible> {
    #[snafu(display("failed to resolve active initializer input"))]
    Resolve { source: E },

    #[snafu(display("memory byte size overflow"))]
    MemoryByteSizeOverflow,

    #[snafu(display("global {index:?} has no constant initializer"))]
    MissingGlobalInitializer { index: GlobalIndex },

    #[snafu(display("malformed const expression: {reason}"))]
    MalformedConstExpr { reason: String },

    #[snafu(display("unsupported active segment offset expression {op}"))]
    UnsupportedActiveOffsetExpression { op: String },
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
            .checked_mul(65536)
            .context(MemoryByteSizeOverflowSnafu)?;
        let offset = active_offset(module, resolver, init.base, init.offset)?;
        let end = offset.saturating_add(init.data.len() as u64);
        if offset > byte_len || end > byte_len {
            return Ok(true);
        }
    }

    for init in &module.info.table_initializers {
        let min_elements = resolver
            .table_min_elements(module, init.table_index)
            .context(ResolveSnafu)?;
        let offset = active_offset(module, resolver, init.base, u64::from(init.offset))?;
        let end = offset.saturating_add(init.elements.len() as u64);
        if offset > min_elements || end > min_elements {
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
    Ok(global_i32_value(module, resolver, base)?.saturating_add(offset))
}

fn global_i32_value<R: ActiveInitializerResolver>(
    module: &NixModule, resolver: &mut R, index: GlobalIndex,
) -> Result<u64, R::Error> {
    if let Some((module_name, field_name)) = module.info.imported_globals.get(index.index()) {
        return resolver
            .imported_global_i32(module_name, field_name)
            .context(ResolveSnafu);
    }

    let expr = module.info.global_inits[index]
        .as_ref()
        .context(MissingGlobalInitializerSnafu { index })?;
    eval_i32_const_expr(module, resolver, expr)
}

fn eval_i32_const_expr<R: ActiveInitializerResolver>(
    module: &NixModule, resolver: &mut R, expr: &ConstExpr,
) -> Result<u64, R::Error> {
    let mut stack = Vec::new();
    for op in expr.ops().iter().copied() {
        match op {
            ConstOp::I32Const(value) => stack.push(u64::from(value.cast_unsigned())),
            ConstOp::GlobalGet(index) => stack.push(global_i32_value(module, resolver, index)?),
            ConstOp::I32Add | ConstOp::I32Sub | ConstOp::I32Mul => {
                let rhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: format!("{op:?} missing right operand"),
                })?;
                let lhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: format!("{op:?} missing left operand"),
                })?;
                let (lhs, rhs) = (lhs as u32, rhs as u32);
                let value = match op {
                    ConstOp::I32Add => lhs.wrapping_add(rhs),
                    ConstOp::I32Sub => lhs.wrapping_sub(rhs),
                    _ => lhs.wrapping_mul(rhs),
                };
                stack.push(u64::from(value));
            }
            _ => {
                return UnsupportedActiveOffsetExpressionSnafu {
                    op: format!("{op:?}"),
                }
                .fail();
            }
        }
    }

    ensure!(
        stack.len() == 1,
        MalformedConstExprSnafu {
            reason: "active segment offset expression must produce one result".to_string()
        }
    );
    stack.first().copied().context(MalformedConstExprSnafu {
        reason: "active segment offset expression produced no result".to_string(),
    })
}
