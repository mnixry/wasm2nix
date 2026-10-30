use cranelift_wasm::ConstOp;
use cranelift_wasm::WasmValType;
use snafu::OptionExt;
use wasm2nix_runtime::RuntimeFunction;

use crate::lower::error::ConstExprOperandSnafu;
use crate::lower::error::ConstExprResultsSnafu;
use crate::lower::error::Result;
use crate::lower::error::UnsupportedConstOpSnafu;
use crate::lower::names::global_bind_name;
use crate::lower::table_entry::lower_table_entry;
use crate::nix::Expr;
use crate::translator::NixModule;

pub(super) fn lower_const_expr(
    module: &NixModule, expr: &cranelift_wasm::ConstExpr, ty: WasmValType,
) -> Result<Expr> {
    let mut stack = Vec::new();

    for op in expr.ops().iter().copied() {
        match op {
            ConstOp::I32Const(value) => stack.push(Expr::int(i64::from(value.cast_unsigned()))),
            ConstOp::I64Const(value) => stack.push(Expr::int(value)),
            ConstOp::F32Const(bits) => stack.push(Expr::int(i64::from(bits))),
            ConstOp::F64Const(bits) => stack.push(Expr::int(bits.cast_signed())),
            ConstOp::GlobalGet(idx) => stack.push(Expr::ident(global_bind_name(idx))),
            ConstOp::RefNull if matches!(ty, WasmValType::Ref(_)) => stack.push(Expr::int(0)),
            ConstOp::RefFunc(func_index) if matches!(ty, WasmValType::Ref(_)) => {
                stack.push(lower_table_entry(module, func_index));
            }
            ConstOp::I32Add
            | ConstOp::I32Sub
            | ConstOp::I32Mul
            | ConstOp::I64Add
            | ConstOp::I64Sub
            | ConstOp::I64Mul => {
                let (rhs, lhs) = stack
                    .pop()
                    .zip(stack.pop())
                    .context(ConstExprOperandSnafu { op })?;
                let function = match op {
                    ConstOp::I32Add => RuntimeFunction::I32Add,
                    ConstOp::I32Sub => RuntimeFunction::I32Sub,
                    ConstOp::I32Mul => RuntimeFunction::I32Mul,
                    ConstOp::I64Add => RuntimeFunction::I64Add,
                    ConstOp::I64Sub => RuntimeFunction::I64Sub,
                    _ => RuntimeFunction::I64Mul,
                };
                stack.push(function.call([lhs, rhs]));
            }
            _ => return UnsupportedConstOpSnafu { op }.fail(),
        }
    }

    let count = stack.len();
    let [result] = <[Expr; 1]>::try_from(stack)
        .ok()
        .context(ConstExprResultsSnafu { count })?;
    Ok(result)
}
