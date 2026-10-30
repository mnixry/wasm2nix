use crate::lower::inst::Force;
use crate::lower::inst::Step;
use crate::lower::inst::is_bit_op;
use crate::nix::BinOp;
use crate::nix::Expr;
use crate::nix::LetBind;

pub(super) struct LoweredBlockBody {
    pub(super) binds: Vec<LetBind>,
    pub(super) forces: Vec<Force>,
    pub(super) body: Expr,
}

pub(super) fn inline_single_use_values(steps: Vec<Step>, body: Expr) -> LoweredBlockBody {
    let mut binds = Vec::new();
    let mut forces = Vec::new();

    for step in steps {
        binds.extend(step.binds);
        forces.extend(step.force);
    }

    let mut body = body;
    let mut index = 0;
    while index < binds.len() {
        if !is_inline_candidate(&binds[index].value)
            || uses_in_binds(&binds[..index], &binds[index].name) != 0
        {
            index += 1;
            continue;
        }
        let name = binds[index].name.clone();

        // Forcing a bound value is redundant once it is inlined; trap
        // conditions are ordinary use sites.
        let removable_force_count = forces
            .iter()
            .filter(|force| matches!(force, Force::Value(expr) if is_ident_named(expr, &name)))
            .count();
        if removable_force_count > 1 {
            index += 1;
            continue;
        }

        let use_count = uses_in_binds(&binds[index + 1..], &name) + uses_in_forces(&forces, &name)
            - removable_force_count
            + body.free_uses(&name);
        // List elements and attribute values stay unevaluated, so an expression
        // inlined into a jump record would grow a thunk chain across loop
        // iterations instead of being forced in this block.
        if use_count > 1 || is_used_lazily(&body, &name) {
            index += 1;
            continue;
        }

        if removable_force_count == 1 {
            remove_ident_force(&mut forces, &name);
        }
        let value = binds.remove(index).value;
        if use_count == 1 {
            for bind in &mut binds[index..] {
                bind.value.substitute_free(&name, &value);
            }
            for force in &mut forces {
                force.expr_mut().substitute_free(&name, &value);
            }
            body.substitute_free(&name, &value);
        }
    }

    LoweredBlockBody {
        binds,
        forces,
        body,
    }
}

fn is_inline_candidate(expr: &Expr) -> bool {
    match expr {
        Expr::Ident(_) | Expr::Int(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Null => true,
        Expr::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            is_inline_candidate(cond)
                && is_inline_candidate(then_expr)
                && is_inline_candidate(else_expr)
        }
        // Division by a non-zero literal cannot trap.
        Expr::BinOp {
            op: BinOp::Div,
            lhs,
            rhs,
        } => {
            matches!(rhs.as_ref(), Expr::Int(divisor) if *divisor != 0) && is_inline_candidate(lhs)
        }
        Expr::BinOp { op, lhs, rhs } => {
            is_safe_inline_binop(*op) && is_inline_candidate(lhs) && is_inline_candidate(rhs)
        }
        Expr::UnOp { expr, .. } => is_inline_candidate(expr),
        Expr::Apply { func, args } => is_bit_op(func) && args.iter().all(is_inline_candidate),
        Expr::Path(_)
        | Expr::List(_)
        | Expr::AttrSet { .. }
        | Expr::Select { .. }
        | Expr::Lambda { .. }
        | Expr::LetIn { .. }
        | Expr::Assert { .. } => false,
    }
}

const fn is_safe_inline_binop(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Eq
            | BinOp::Neq
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
            | BinOp::And
            | BinOp::Or
    )
}

fn uses_in_binds(binds: &[LetBind], name: &str) -> usize {
    binds.iter().map(|bind| bind.value.free_uses(name)).sum()
}

fn uses_in_forces(forces: &[Force], name: &str) -> usize {
    forces
        .iter()
        .map(|force| force.expr().free_uses(name))
        .sum()
}

/// Whether a free `name` occurs inside a list element or an attribute value.
/// Those positions stay unevaluated, so an expression inlined there would be
/// carried as a thunk instead of being forced in this block.
fn is_used_lazily(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::List(_) | Expr::AttrSet { .. } => expr.free_uses(name) != 0,
        _ if expr.binds(name) => false,
        _ => {
            let mut found = false;
            expr.for_each_child(|child| found = found || is_used_lazily(child, name));
            found
        }
    }
}

fn is_ident_named(expr: &Expr, name: &str) -> bool {
    matches!(expr, Expr::Ident(ident) if ident == name)
}

fn remove_ident_force(forces: &mut Vec<Force>, name: &str) {
    let mut removed = false;
    forces.retain(|force| {
        if !removed && matches!(force, Force::Value(expr) if is_ident_named(expr, name)) {
            removed = true;
            false
        } else {
            true
        }
    });
}
