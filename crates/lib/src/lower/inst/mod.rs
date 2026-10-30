//! Lowering of non-terminator instructions, by instruction family.

mod arith;
mod call;
mod compare;
mod convert;
mod ctx;
mod memory;
mod step;

pub(super) use arith::is_bit_op;
pub(super) use ctx::InstCtx;
pub(super) use ctx::truthy_value_expr;
pub(super) use ctx::value_expr;
pub(super) use step::Force;
pub(super) use step::Step;
