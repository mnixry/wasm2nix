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
    Update, // //
}

impl BinOp {
    const fn as_str(self) -> &'static str {
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
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
}

impl UnOp {
    const fn as_str(self) -> &'static str {
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
        rec: bool,
        binds: Vec<(AttrKey, Self)>,
    },

    Select {
        expr: Box<Self>,
        attr: AttrKey,
    },
    Lambda {
        param: String,
        body: Box<Self>,
    },
    LetIn {
        binds: Vec<(String, Self)>,
        body: Box<Self>,
    },
    IfThenElse {
        cond: Box<Self>,
        then_expr: Box<Self>,
        else_expr: Box<Self>,
    },

    Apply {
        func: Box<Self>,
        arg: Box<Self>,
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
    pub const fn attrset(rec: bool, binds: Vec<(AttrKey, Self)>) -> Self {
        Self::AttrSet { rec, binds }
    }

    #[must_use]
    pub fn select(expr: Self, attr: AttrKey) -> Self {
        Self::Select {
            expr: Box::new(expr),
            attr,
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
        Self::Apply {
            func: Box::new(func),
            arg: Box::new(arg),
        }
    }

    pub fn app(mut func: Self, args: impl IntoIterator<Item = Self>) -> Self {
        for arg in args {
            func = Self::apply(func, arg);
        }
        func
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
        let mut p = Printer::new();
        p.expr(self, Prec::Top);
        p.finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    Top         = 0,
    LetIfLambda = 10,
    Or          = 20,
    And         = 30,
    Cmp         = 40,
    Add         = 50,
    Mul         = 60,
    Apply       = 70,
    Select      = 80,
    Atom        = 90,
}

impl Expr {
    const fn prec(&self) -> Prec {
        match self {
            Self::LetIn { .. } | Self::IfThenElse { .. } | Self::Lambda { .. } => Prec::LetIfLambda,
            Self::BinOp { op, .. } => match op {
                BinOp::Or => Prec::Or,
                BinOp::And => Prec::And,
                BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Prec::Cmp,
                BinOp::Add | BinOp::Sub | BinOp::Update => Prec::Add,
                BinOp::Mul | BinOp::Div | BinOp::Mod => Prec::Mul,
            },
            Self::Apply { .. } => Prec::Apply,
            Self::Select { .. } => Prec::Select,
            Self::UnOp { .. } => Prec::Mul,
            Self::Ident(_)
            | Self::Raw(_)
            | Self::Path(_)
            | Self::Int(_)
            | Self::Bool(_)
            | Self::Str(_)
            | Self::Null
            | Self::List(_)
            | Self::AttrSet { .. } => Prec::Atom,
        }
    }
}

struct Printer {
    out: String,
    indent: usize,
}

impl Printer {
    const fn new() -> Self {
        Self {
            out: String::new(),
            indent: 0,
        }
    }

    fn finish(self) -> String {
        self.out
    }

    fn push(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn nl(&mut self) {
        self.out.push('\n');
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
    }

    fn parens_if(&mut self, need: bool, f: impl FnOnce(&mut Self)) {
        if need {
            self.push("(");
        }
        f(self);
        if need {
            self.push(")");
        }
    }

    fn attr_key(&mut self, k: &AttrKey) {
        match k {
            AttrKey::Ident(s) => self.push(s),
            AttrKey::Str(s) => {
                self.push("\"");
                self.push(&escape_string(s));
                self.push("\"");
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn expr(&mut self, e: &Expr, parent: Prec) {
        let my_prec = e.prec();
        let need_parens = my_prec < parent;

        self.parens_if(need_parens, |p| match e {
            Expr::Ident(s) | Expr::Raw(s) | Expr::Path(s) => p.push(s),
            Expr::Int(v) if *v == i64::MIN => p.push("(0 - 9223372036854775807 - 1)"),
            Expr::Int(v) if *v < 0 => {
                p.push("(");
                p.push(&v.to_string());
                p.push(")");
            }
            Expr::Int(v) => p.push(&v.to_string()),
            Expr::Bool(true) => p.push("true"),
            Expr::Bool(false) => p.push("false"),
            Expr::Str(s) => {
                p.push("\"");
                p.push(&escape_string(s));
                p.push("\"");
            }
            Expr::Null => p.push("null"),
            Expr::List(items) => {
                p.push("[");
                if !items.is_empty() {
                    p.push(" ");
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            p.push(" ");
                        }
                        p.expr(item, Prec::Atom);
                    }
                    p.push(" ");
                }
                p.push("]");
            }
            Expr::AttrSet { rec, binds } => {
                if *rec {
                    p.push("rec ");
                }
                if binds.is_empty() {
                    p.push("{ }");
                    return;
                }
                p.push("{");
                p.indent += 1;
                for (k, v) in binds {
                    p.nl();
                    p.attr_key(k);
                    p.push(" = ");
                    p.expr(v, Prec::Top);
                    p.push(";");
                }
                p.indent -= 1;
                p.nl();
                p.push("}");
            }
            Expr::Select { expr, attr } => {
                p.expr(expr, Prec::Select);
                p.push(".");
                p.attr_key(attr);
            }
            Expr::Lambda { param, body } => {
                p.push(param);
                p.push(": ");
                p.expr(body, Prec::LetIfLambda);
            }
            Expr::LetIn { binds, body } => {
                if binds.is_empty() {
                    p.expr(body, Prec::Top);
                    return;
                }
                p.push("let");
                p.indent += 1;
                for (k, v) in binds {
                    p.nl();
                    p.push(k);
                    p.push(" = ");
                    p.expr(v, Prec::Top);
                    p.push(";");
                }
                p.indent -= 1;
                p.nl();
                p.push("in ");
                p.expr(body, Prec::LetIfLambda);
            }
            Expr::IfThenElse {
                cond,
                then_expr,
                else_expr,
            } => {
                p.push("if ");
                p.expr(cond, Prec::Top);
                p.push(" then ");
                p.expr(then_expr, Prec::LetIfLambda);
                p.push(" else ");
                p.expr(else_expr, Prec::LetIfLambda);
            }
            Expr::Apply { func, arg } => {
                p.expr(func, Prec::Apply);
                p.push(" ");
                p.expr(arg, Prec::Select);
            }
            Expr::BinOp { op, lhs, rhs } => {
                let prec = my_prec;
                p.expr(lhs, prec);
                p.push(" ");
                p.push(op.as_str());
                p.push(" ");
                p.expr(rhs, prec);
            }
            Expr::UnOp { op, expr } => {
                p.push(op.as_str());
                p.expr(expr, Prec::Mul);
            }
        });
    }
}

fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_attrset_and_let() {
        let e = Expr::let_in(
            vec![
                ("x".into(), Expr::int(1)),
                (
                    "y".into(),
                    Expr::bin(BinOp::Add, Expr::ident("x"), Expr::int(2)),
                ),
            ],
            Expr::attrset(
                false,
                vec![
                    (AttrKey::ident("x"), Expr::ident("x")),
                    (AttrKey::ident("y"), Expr::ident("y")),
                ],
            ),
        );

        let s = e.render();
        assert!(s.contains("let"));
        assert!(s.contains("in"));
        assert!(s.contains('{'));
        assert!(s.contains("x = 1;"));
    }
}
