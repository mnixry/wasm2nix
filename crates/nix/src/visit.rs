//! Traversal of [`Expr`] trees in the style of `syn::visit`: override the
//! hooks you care about and call the matching `walk_*` function to recurse.

use crate::Expr;

/// Visits an expression tree by shared reference.
pub trait Visit<'ast> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        walk_expr(self, expr);
    }
}

/// Visits every direct child expression of `expr`.
pub fn walk_expr<'ast, V: Visit<'ast> + ?Sized>(visitor: &mut V, expr: &'ast Expr) {
    match expr {
        Expr::Ident(_)
        | Expr::Raw(_)
        | Expr::Path(_)
        | Expr::Int(_)
        | Expr::Bool(_)
        | Expr::Str(_)
        | Expr::Null => {}
        Expr::List(items) => {
            for item in items {
                visitor.visit_expr(item);
            }
        }
        Expr::AttrSet { binds, .. } => {
            for bind in binds {
                visitor.visit_expr(&bind.value);
            }
        }
        Expr::Select { expr, .. } | Expr::UnOp { expr, .. } => visitor.visit_expr(expr),
        Expr::Lambda { body, .. } => visitor.visit_expr(body),
        Expr::LetIn { binds, body } => {
            for bind in binds {
                visitor.visit_expr(&bind.value);
            }
            visitor.visit_expr(body);
        }
        Expr::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            visitor.visit_expr(cond);
            visitor.visit_expr(then_expr);
            visitor.visit_expr(else_expr);
        }
        Expr::Assert { conds, body } => {
            for cond in conds {
                visitor.visit_expr(cond);
            }
            visitor.visit_expr(body);
        }
        Expr::Apply { func, args } => {
            visitor.visit_expr(func);
            for arg in args {
                visitor.visit_expr(arg);
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            visitor.visit_expr(lhs);
            visitor.visit_expr(rhs);
        }
    }
}

/// Visits an expression tree by mutable reference.
pub trait VisitMut {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        walk_expr_mut(self, expr);
    }
}

/// Visits every direct child expression of `expr` mutably.
pub fn walk_expr_mut<V: VisitMut + ?Sized>(visitor: &mut V, expr: &mut Expr) {
    match expr {
        Expr::Ident(_)
        | Expr::Raw(_)
        | Expr::Path(_)
        | Expr::Int(_)
        | Expr::Bool(_)
        | Expr::Str(_)
        | Expr::Null => {}
        Expr::List(items) => {
            for item in items {
                visitor.visit_expr_mut(item);
            }
        }
        Expr::AttrSet { binds, .. } => {
            for bind in binds {
                visitor.visit_expr_mut(&mut bind.value);
            }
        }
        Expr::Select { expr, .. } | Expr::UnOp { expr, .. } => visitor.visit_expr_mut(expr),
        Expr::Lambda { body, .. } => visitor.visit_expr_mut(body),
        Expr::LetIn { binds, body } => {
            for bind in binds {
                visitor.visit_expr_mut(&mut bind.value);
            }
            visitor.visit_expr_mut(body);
        }
        Expr::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => {
            visitor.visit_expr_mut(cond);
            visitor.visit_expr_mut(then_expr);
            visitor.visit_expr_mut(else_expr);
        }
        Expr::Assert { conds, body } => {
            for cond in conds {
                visitor.visit_expr_mut(cond);
            }
            visitor.visit_expr_mut(body);
        }
        Expr::Apply { func, args } => {
            visitor.visit_expr_mut(func);
            for arg in args {
                visitor.visit_expr_mut(arg);
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            visitor.visit_expr_mut(lhs);
            visitor.visit_expr_mut(rhs);
        }
    }
}
