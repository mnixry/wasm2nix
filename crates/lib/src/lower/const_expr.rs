use cranelift_wasm::{ConstOp, WasmValType};
use snafu::prelude::*;
use wasm2nix_runtime::RuntimeFunction;

use crate::{
    lower::{
        error::{MalformedConstExprSnafu, Result},
        state::{global_bind_name, lower_table_entry},
    },
    nix::Expr,
    translator::NixModule,
};

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
                let rhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: format!("{op:?} missing right operand"),
                })?;
                let lhs = stack.pop().context(MalformedConstExprSnafu {
                    reason: format!("{op:?} missing left operand"),
                })?;
                let function = match op {
                    ConstOp::I32Add => RuntimeFunction::I32Add,
                    ConstOp::I32Sub => RuntimeFunction::I32Sub,
                    ConstOp::I32Mul => RuntimeFunction::I32Mul,
                    ConstOp::I64Add => RuntimeFunction::I64Add,
                    ConstOp::I64Sub => RuntimeFunction::I64Sub,
                    _ => RuntimeFunction::I64Mul,
                };
                stack.push(function.call(vec![lhs, rhs]));
            }
            _ => {
                return MalformedConstExprSnafu {
                    reason: format!("unsupported operator {op:?}"),
                }
                .fail();
            }
        }
    }

    ensure!(
        stack.len() == 1,
        MalformedConstExprSnafu {
            reason: format!("expected one result, found {}", stack.len()),
        }
    );
    stack.pop().context(MalformedConstExprSnafu {
        reason: "empty stack".to_string(),
    })
}
