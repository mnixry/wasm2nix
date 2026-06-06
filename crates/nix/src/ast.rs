use std::fmt;

use crate::format::Printer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrKey {
    Ident(String),
    Str(String),
}

impl AttrKey {
    pub fn ident(name: impl Into<String>) -> Self {
        Self::Ident(name.into())
    }

    pub fn string(s: impl Into<String>) -> Self {
        Self::Str(s.into())
    }
}

impl From<&str> for AttrKey {
    fn from(value: &str) -> Self {
        Self::ident(value)
    }
}

impl From<String> for AttrKey {
    fn from(value: String) -> Self {
        Self::ident(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrPath(Vec<AttrKey>);

impl AttrPath {
    #[must_use]
    pub const fn new(keys: Vec<AttrKey>) -> Self {
        Self(keys)
    }

    #[must_use]
    pub fn one(key: AttrKey) -> Self {
        Self(vec![key])
    }

    pub fn push(&mut self, key: AttrKey) {
        self.0.push(key);
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &AttrKey> {
        self.0.iter()
    }

    #[must_use]
    pub fn into_vec(self) -> Vec<AttrKey> {
        self.0
    }
}

impl From<AttrKey> for AttrPath {
    fn from(value: AttrKey) -> Self {
        Self::one(value)
    }
}

impl From<Vec<AttrKey>> for AttrPath {
    fn from(value: Vec<AttrKey>) -> Self {
        Self::new(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrBind {
    pub path: AttrPath,
    pub value: Expr,
}

impl AttrBind {
    #[must_use]
    pub fn new(path: impl Into<AttrPath>, value: Expr) -> Self {
        Self {
            path: path.into(),
            value,
        }
    }
}

impl From<(AttrKey, Expr)> for AttrBind {
    fn from((key, value): (AttrKey, Expr)) -> Self {
        Self::new(key, value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetBind {
    pub name: String,
    pub value: Expr,
}

impl LetBind {
    pub fn new(name: impl Into<String>, value: Expr) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }
}

impl From<(String, Expr)> for LetBind {
    fn from((name, value): (String, Expr)) -> Self {
        Self::new(name, value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Update,
    Concat,
    Implies,
    PipeForward,
    PipeBackward,
}

impl BinOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Mod => "%",
            Self::Eq => "==",
            Self::Neq => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::And => "&&",
            Self::Or => "||",
            Self::Update => "//",
            Self::Concat => "++",
            Self::Implies => "->",
            Self::PipeForward => "|>",
            Self::PipeBackward => "<|",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
}

impl UnOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Not => "!",
            Self::Neg => "-",
        }
    }
}

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

    #[must_use]
    pub fn select_path(expr: Self, path: impl Into<AttrPath>) -> Self {
        let path = path.into();
        if path.is_empty() {
            return expr;
        }

        match expr {
            Self::Select {
                expr,
                path: mut existing,
            } => {
                for key in path.into_vec() {
                    existing.push(key);
                }
                Self::Select {
                    expr,
                    path: existing,
                }
            }
            expr => Self::Select {
                expr: Box::new(expr),
                path,
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
    pub fn apply(func: Self, arg: Self) -> Self {
        match func {
            Self::Apply { func, mut args } => {
                args.push(arg);
                Self::Apply { func, args }
            }
            func => Self::Apply {
                func: Box::new(func),
                args: vec![arg],
            },
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

pub trait IntoNixExpr {
    fn into_nix_expr(self) -> Expr;
}

impl IntoNixExpr for Expr {
    fn into_nix_expr(self) -> Expr {
        self
    }
}

impl IntoNixExpr for &Expr {
    fn into_nix_expr(self) -> Expr {
        self.clone()
    }
}

impl IntoNixExpr for bool {
    fn into_nix_expr(self) -> Expr {
        Expr::bool(self)
    }
}

impl IntoNixExpr for String {
    fn into_nix_expr(self) -> Expr {
        Expr::string(self)
    }
}

impl IntoNixExpr for &String {
    fn into_nix_expr(self) -> Expr {
        Expr::string(self.clone())
    }
}

impl IntoNixExpr for &str {
    fn into_nix_expr(self) -> Expr {
        Expr::string(self)
    }
}

impl<T: IntoNixExpr> IntoNixExpr for Option<T> {
    fn into_nix_expr(self) -> Expr {
        self.map_or_else(Expr::null, IntoNixExpr::into_nix_expr)
    }
}

macro_rules! impl_int_expr {
    ($($ty:ty),* $(,)?) => {
        $(
            impl IntoNixExpr for $ty {
                fn into_nix_expr(self) -> Expr {
                    Expr::int(i64::from(self))
                }
            }
        )*
    };
}

impl_int_expr!(i8, i16, i32, i64, u8, u16, u32);

impl IntoNixExpr for usize {
    fn into_nix_expr(self) -> Expr {
        Expr::int(i64::try_from(self).expect("usize Nix literal fits in i64"))
    }
}

impl IntoNixExpr for u64 {
    fn into_nix_expr(self) -> Expr {
        Expr::int(i64::try_from(self).expect("u64 Nix literal fits in i64"))
    }
}

impl IntoNixExpr for isize {
    fn into_nix_expr(self) -> Expr {
        Expr::int(i64::try_from(self).expect("isize Nix literal fits in i64"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    pub width: usize,
    pub indent: usize,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            width: 100,
            indent: 2,
        }
    }
}
