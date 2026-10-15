use crate::{BinOp, Expr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Prec {
    Top     = 0,
    Implies = 30,
    Or      = 40,
    And     = 50,
    Equal   = 60,
    Compare = 70,
    Update  = 80,
    Prefix  = 90,
    Add     = 100,
    Mul     = 110,
    Concat  = 120,
    Apply   = 130,
    Select  = 140,
    Atom    = 150,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Assoc {
    Left,
    Right,
    None,
}

impl From<BinOp> for Prec {
    fn from(val: BinOp) -> Self {
        match val {
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
}

impl From<BinOp> for Assoc {
    fn from(val: BinOp) -> Self {
        match val {
            BinOp::Update | BinOp::Concat | BinOp::Implies => Self::Right,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::And | BinOp::Or => {
                Self::Left
            }
            BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Self::None,
        }
    }
}

impl From<&Expr> for Prec {
    fn from(val: &Expr) -> Self {
        match val {
            Expr::LetIn { .. }
            | Expr::IfThenElse { .. }
            | Expr::Assert { .. }
            | Expr::Lambda { .. } => Self::Top,
            Expr::BinOp { op, .. } => (*op).into(),
            Expr::UnOp { .. } => Self::Prefix,
            Expr::Apply { .. } => Self::Apply,
            Expr::Select { .. } => Self::Select,
            Expr::Ident(_)
            | Expr::Raw(_)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildSide {
    Left,
    Right,
}
