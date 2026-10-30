use std::fmt;

use super::AttrBind;
use super::AttrKey;
use super::AttrPath;
use super::BinOp;
use super::LetBind;
use super::UnOp;
use crate::print::RenderOptions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Ident(String),
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

    pub fn attrset(binds: impl IntoIterator<Item = AttrBind>) -> Self {
        Self::AttrSet {
            recursive: false,
            binds: binds.into_iter().collect(),
        }
    }

    pub fn rec_attrset(binds: impl IntoIterator<Item = AttrBind>) -> Self {
        Self::AttrSet {
            recursive: true,
            binds: binds.into_iter().collect(),
        }
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

    /// A `let` over `binds`; just `body` when there are none.
    pub fn let_in(binds: impl IntoIterator<Item = LetBind>, body: Self) -> Self {
        let binds = binds.into_iter().collect::<Vec<_>>();
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
        crate::print::render(self, options)
    }
}

/// Calls `$f` on every direct child of `$expr`; `mut` borrows them mutably.
macro_rules! for_each_child {
    ($expr:expr, $f:ident $(, $mut:tt)?) => {
        match $expr {
            Expr::Ident(_)
            | Expr::Path(_)
            | Expr::Int(_)
            | Expr::Bool(_)
            | Expr::Str(_)
            | Expr::Null => {}
            Expr::List(items) => {
                for item in items {
                    $f(item);
                }
            }
            Expr::AttrSet { binds, .. } => {
                for bind in binds {
                    $f(& $($mut)? bind.value);
                }
            }
            Expr::Select { expr, .. } | Expr::UnOp { expr, .. } => $f(expr),
            Expr::Lambda { body, .. } => $f(body),
            Expr::LetIn { binds, body } => {
                for bind in binds {
                    $f(& $($mut)? bind.value);
                }
                $f(body);
            }
            Expr::IfThenElse {
                cond,
                then_expr,
                else_expr,
            } => {
                $f(cond);
                $f(then_expr);
                $f(else_expr);
            }
            Expr::Assert { conds, body } => {
                for cond in conds {
                    $f(cond);
                }
                $f(body);
            }
            Expr::Apply { func, args } => {
                $f(func);
                for arg in args {
                    $f(arg);
                }
            }
            Expr::BinOp { lhs, rhs, .. } => {
                $f(lhs);
                $f(rhs);
            }
        }
    };
}

impl Expr {
    /// Calls `f` on every direct child expression.
    pub fn for_each_child<'a>(&'a self, mut f: impl FnMut(&'a Self)) {
        for_each_child!(self, f);
    }

    /// Calls `f` on every direct child expression, mutably.
    pub fn for_each_child_mut(&mut self, mut f: impl FnMut(&mut Self)) {
        for_each_child!(self, f, mut);
    }
}

impl From<bool> for Expr {
    fn from(value: bool) -> Self {
        Self::bool(value)
    }
}

impl From<&str> for Expr {
    fn from(value: &str) -> Self {
        Self::string(value)
    }
}

impl From<String> for Expr {
    fn from(value: String) -> Self {
        Self::string(value)
    }
}

impl From<&String> for Expr {
    fn from(value: &String) -> Self {
        Self::string(value.as_str())
    }
}

impl<T: Into<Self>> From<Option<T>> for Expr {
    fn from(value: Option<T>) -> Self {
        value.map_or_else(Self::null, Into::into)
    }
}

macro_rules! impl_from_int {
    ($($ty:ty),*) => {
        $(
            impl From<$ty> for Expr {
                fn from(value: $ty) -> Self {
                    Self::int(i64::from(value))
                }
            }
        )*
    };
}

impl_from_int!(i8, i16, i32, i64, u8, u16, u32);

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}
