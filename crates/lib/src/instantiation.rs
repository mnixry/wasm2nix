use cranelift_entity::EntityRef;
use cranelift_wasm::{ConstExpr, ConstOp, GlobalIndex, MemoryIndex, TableIndex};
use snafu::prelude::*;

use crate::translator::NixModule;

#[derive(Debug, Snafu)]
#[snafu(display("{message}"))]
pub struct ResolverError {
    message: String,
}

impl ResolverError {
    const fn new(message: String) -> Self {
        Self { message }
    }
}

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum InstantiationError {
    #[snafu(display("failed to resolve active initializer input"))]
    Resolve { source: ResolverError },

    #[snafu(display("memory byte size overflow"))]
    MemoryByteSizeOverflow,

    #[snafu(display("global {index:?} has no constant initializer"))]
    MissingGlobalInitializer { index: GlobalIndex },

    #[snafu(display("malformed const expression: {reason}"))]
    MalformedConstExpr { reason: String },

    #[snafu(display("unsupported active segment offset expression {op}"))]
    UnsupportedActiveOffsetExpression { op: String },
}

pub type Result<T, E = InstantiationError> = std::result::Result<T, E>;

pub trait ActiveInitializerResolver {
    /// Returns the minimum page count for a memory.
    ///
    /// # Errors
    ///
    /// Returns an error when the memory cannot be resolved.
    fn memory_min_pages(&mut self, module: &NixModule, index: MemoryIndex) -> Result<u64, String>;

    /// Returns the minimum element count for a table.
    ///
    /// # Errors
    ///
    /// Returns an error when the table cannot be resolved.
    fn table_min_elements(&mut self, module: &NixModule, index: TableIndex) -> Result<u64, String>;

    /// Returns the current value of an imported i32 global.
    ///
    /// # Errors
    ///
    /// Returns an error when the imported global cannot be resolved.
    fn imported_global_i32(&mut self, module_name: &str, field_name: &str) -> Result<u64, String>;
}

/// Reports whether active data or element initializers would trap during
/// instantiation.
///
/// # Errors
///
/// Returns an error when an imported initializer input cannot be resolved, when
/// initializer arithmetic overflows, or when an initializer expression is
/// malformed or unsupported.
pub fn active_initializers_trap(
    module: &NixModule, resolver: &mut impl ActiveInitializerResolver,
) -> Result<bool> {
    for init in &module.info.data_initializers {
        let min_pages = resolver
            .memory_min_pages(module, init.memory_index)
            .map_err(ResolverError::new)
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
            .map_err(ResolverError::new)
            .context(ResolveSnafu)?;
        let offset = active_offset(module, resolver, init.base, u64::from(init.offset))?;
        let end = offset.saturating_add(init.elements.len() as u64);
        if offset > min_elements || end > min_elements {
            return Ok(true);
        }
    }

    Ok(false)
}

fn active_offset(
    module: &NixModule, resolver: &mut impl ActiveInitializerResolver, base: Option<GlobalIndex>,
    offset: u64,
) -> Result<u64> {
    let Some(base) = base else {
        return Ok(offset);
    };
    Ok(global_i32_value(module, resolver, base)?.saturating_add(offset))
}

fn global_i32_value(
    module: &NixModule, resolver: &mut impl ActiveInitializerResolver, index: GlobalIndex,
) -> Result<u64> {
    if let Some((module_name, field_name)) = module.info.imported_globals.get(index.index()) {
        return resolver
            .imported_global_i32(module_name, field_name)
            .map_err(ResolverError::new)
            .context(ResolveSnafu);
    }

    let expr = module.info.global_inits[index]
        .as_ref()
        .context(MissingGlobalInitializerSnafu { index })?;
    eval_i32_const_expr(module, resolver, expr)
}

fn eval_i32_const_expr(
    module: &NixModule, resolver: &mut impl ActiveInitializerResolver, expr: &ConstExpr,
) -> Result<u64> {
    let mut stack = Vec::new();
    for op in expr.ops().iter().copied() {
        match op {
            ConstOp::I32Const(value) => stack.push(u64::from(value.cast_unsigned())),
            ConstOp::GlobalGet(index) => stack.push(global_i32_value(module, resolver, index)?),
            ConstOp::I32Add => {
                let rhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: "i32.add missing right operand".to_string(),
                })?;
                let lhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: "i32.add missing left operand".to_string(),
                })?;
                stack.push(u64::from((lhs as u32).wrapping_add(rhs as u32)));
            }
            ConstOp::I32Sub => {
                let rhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: "i32.sub missing right operand".to_string(),
                })?;
                let lhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: "i32.sub missing left operand".to_string(),
                })?;
                stack.push(u64::from((lhs as u32).wrapping_sub(rhs as u32)));
            }
            ConstOp::I32Mul => {
                let rhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: "i32.mul missing right operand".to_string(),
                })?;
                let lhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: "i32.mul missing left operand".to_string(),
                })?;
                stack.push(u64::from((lhs as u32).wrapping_mul(rhs as u32)));
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
