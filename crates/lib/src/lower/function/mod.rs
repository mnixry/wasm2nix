//! Lowering of one function: control-flow analysis, blocks, terminators and
//! the single-use inliner.

mod address;
mod block;
mod cfg;
mod inline;
mod lower;
mod terminator;

pub(super) use block::BlockLowering;
pub(super) use lower::lower_function;
