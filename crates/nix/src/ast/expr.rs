use std::fmt;

use super::{AttrBind, AttrKey, AttrPath, BinOp, LetBind, UnOp};
use crate::print::{Printer, RenderOptions};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Ident(String),
    Raw(String),
    Path(String),
    Int(i64),
    Bool(bool),
    Str(String),
    Null,

    List(Vec<Self>),
    AttrSet {
        recursive: bool,
        binds: Vec<AttrBind>,
    },

    Select {
        expr: Box<Self>,
        path: AttrPath,
    },
    Lambda {
        param: String,
        body: Box<Self>,
    },
    LetIn {
        binds: Vec<LetBind>,
        body: Box<Self>,
    },
    IfThenElse {
        cond: Box<Self>,
        then_expr: Box<Self>,
        else_expr: Box<Self>,
    },
    /// `assert c1; assert c2; body`. Conditions are kept in a list so that long
    /// chains stay flat for the lowering and the printer.
    Assert {
        conds: Vec<Self>,
        body: Box<Self>,
    },

    Apply {
        func: Box<Self>,
        args: Vec<Self>,
    },
    BinOp {
        op: BinOp,
        lhs: Box<Self>,
        rhs: Box<Self>,
    },
    UnOp {
        op: UnOp,
        expr: Box<Self>,
    },
}

impl Expr {
    pub fn ident(name: impl Into<String>) -> Self {
        Self::Ident(name.into())
    }

    pub fn raw(expr: impl Into<String>) -> Self {
        Self::Raw(expr.into())
    }

    pub fn path(path: impl Into<String>) -> Self {
        Self::Path(path.into())
    }

    #[must_use]
    pub const fn int(v: i64) -> Self {
        Self::Int(v)
    }

    #[must_use]
    pub const fn bool(v: bool) -> Self {
        Self::Bool(v)
    }

    pub fn string(v: impl Into<String>) -> Self {
        Self::Str(v.into())
    }

    #[must_use]
    pub const fn null() -> Self {
        Self::Null
    }

    #[must_use]
    pub const fn list(items: Vec<Self>) -> Self {
        Self::List(items)
    }

    #[must_use]
    pub fn attrset(recursive: bool, binds: Vec<(AttrKey, Self)>) -> Self {
        Self::attrs(recursive, binds.into_iter().map(AttrBind::from).collect())
    }

    #[must_use]
    pub const fn attrs(recursive: bool, binds: Vec<AttrBind>) -> Self {
        Self::AttrSet { recursive, binds }
    }

    #[must_use]
    pub fn select(expr: Self, attr: AttrKey) -> Self {
        match expr {
            Self::Select { expr, mut path } => {
                path.push(attr);
                Self::Select { expr, path }
            }
            expr => Self::Select {
                expr: Box::new(expr),
                path: AttrPath::one(attr),
            },
        }
    }

    pub fn lambda(param: impl Into<String>, body: Self) -> Self {
        Self::Lambda {
            param: param.into(),
            body: Box::new(body),
        }
    }

    #[must_use]
    pub fn let_in(binds: Vec<(String, Self)>, body: Self) -> Self {
        Self::let_binds(binds.into_iter().map(LetBind::from).collect(), body)
    }

    #[must_use]
    pub fn let_binds(binds: Vec<LetBind>, body: Self) -> Self {
        if binds.is_empty() {
            return body;
        }

        Self::LetIn {
            binds,
            body: Box::new(body),
        }
    }

    #[must_use]
    pub fn if_then_else(cond: Self, then_expr: Self, else_expr: Self) -> Self {
        Self::IfThenElse {
            cond: Box::new(cond),
            then_expr: Box::new(then_expr),
            else_expr: Box::new(else_expr),
        }
    }

    #[must_use]
    pub fn asserts(conds: Vec<Self>, body: Self) -> Self {
        if conds.is_empty() {
            return body;
        }

        Self::Assert {
            conds,
            body: Box::new(body),
        }
    }

    pub fn app(func: Self, args: impl IntoIterator<Item = Self>) -> Self {
        let mut args = args.into_iter().collect::<Vec<_>>();
        if args.is_empty() {
            return func;
        }

        match func {
            Self::Apply {
                func,
                args: mut existing,
            } => {
                existing.append(&mut args);
                Self::Apply {
                    func,
                    args: existing,
                }
            }
            func => Self::Apply {
                func: Box::new(func),
                args,
            },
        }
    }

    #[must_use]
    pub fn bin(op: BinOp, lhs: Self, rhs: Self) -> Self {
        Self::BinOp {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }

    #[must_use]
    pub fn un(op: UnOp, expr: Self) -> Self {
        Self::UnOp {
            op,
            expr: Box::new(expr),
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        self.render_with(&RenderOptions::default())
    }

    #[must_use]
    pub fn render_with(&self, options: &RenderOptions) -> String {
        let mut printer = Printer::new(options);
        printer.expr(self);
        printer.finish()
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}
