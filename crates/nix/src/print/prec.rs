use crate::BinOp;
use crate::Expr;

/// Binding strength, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Prec {
    Top,
    Implies,
    Or,
    And,
    Equal,
    Compare,
    Update,
    Prefix,
    Add,
    Mul,
    Concat,
    Apply,
    Select,
    Atom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Assoc {
    Left,
    Right,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildSide {
    Left,
    Right,
}

impl Prec {
    pub(super) const fn of_binop(op: BinOp) -> Self {
        match op {
            BinOp::Implies => Self::Implies,
            BinOp::Or => Self::Or,
            BinOp::And => Self::And,
            BinOp::Eq | BinOp::Neq => Self::Equal,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Self::Compare,
            BinOp::Update => Self::Update,
            BinOp::Add | BinOp::Sub => Self::Add,
            BinOp::Mul | BinOp::Div => Self::Mul,
            BinOp::Concat => Self::Concat,
        }
    }

    pub(super) const fn of(expr: &Expr) -> Self {
        match expr {
            Expr::LetIn { .. }
            | Expr::IfThenElse { .. }
            | Expr::Assert { .. }
            | Expr::Lambda { .. } => Self::Top,
            Expr::BinOp { op, .. } => Self::of_binop(*op),
            Expr::UnOp { .. } => Self::Prefix,
            Expr::Apply { .. } => Self::Apply,
            Expr::Select { .. } => Self::Select,
            Expr::Ident(_)
            | Expr::Path(_)
            | Expr::Int(_)
            | Expr::Bool(_)
            | Expr::Str(_)
            | Expr::Null
            | Expr::List(_)
            | Expr::AttrSet { .. } => Self::Atom,
        }
    }
}

impl Assoc {
    pub(super) const fn of_binop(op: BinOp) -> Self {
        match op {
            BinOp::Update | BinOp::Concat | BinOp::Implies => Self::Right,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::And | BinOp::Or => {
                Self::Left
            }
            BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Self::None,
        }
    }
}
