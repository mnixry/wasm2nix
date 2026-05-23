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
    fn as_str(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::Eq => "==",
            BinOp::Neq => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::Update => "//",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
}

impl UnOp {
    fn as_str(self) -> &'static str {
        match self {
            UnOp::Not => "!",
            UnOp::Neg => "-",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Ident(String),
    Int(i64),
    Bool(bool),
    Str(String),
    Null,

    List(Vec<Expr>),
    AttrSet {
        rec: bool,
        binds: Vec<(AttrKey, Expr)>,
    },

    Select {
        expr: Box<Expr>,
        attr: AttrKey,
    },
    Lambda {
        param: String,
        body: Box<Expr>,
    },
    LetIn {
        binds: Vec<(String, Expr)>,
        body: Box<Expr>,
    },
    IfThenElse {
        cond: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },

    Apply {
        func: Box<Expr>,
        arg: Box<Expr>,
    },
    BinOp {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    UnOp {
        op: UnOp,
        expr: Box<Expr>,
    },
}

impl Expr {
    pub fn ident(name: impl Into<String>) -> Self {
        Self::Ident(name.into())
    }

    pub fn int(v: i64) -> Self {
        Self::Int(v)
    }

    pub fn bool(v: bool) -> Self {
        Self::Bool(v)
    }

    pub fn string(v: impl Into<String>) -> Self {
        Self::Str(v.into())
    }

    pub fn null() -> Self {
        Self::Null
    }

    pub fn list(items: Vec<Expr>) -> Self {
        Self::List(items)
    }

    pub fn attrset(rec: bool, binds: Vec<(AttrKey, Expr)>) -> Self {
        Self::AttrSet { rec, binds }
    }

    pub fn select(expr: Expr, attr: AttrKey) -> Self {
        Self::Select {
            expr: Box::new(expr),
            attr,
        }
    }

    pub fn lambda(param: impl Into<String>, body: Expr) -> Self {
        Self::Lambda {
            param: param.into(),
            body: Box::new(body),
        }
    }

    pub fn let_in(binds: Vec<(String, Expr)>, body: Expr) -> Self {
        Self::LetIn {
            binds,
            body: Box::new(body),
        }
    }

    pub fn if_then_else(cond: Expr, then_expr: Expr, else_expr: Expr) -> Self {
        Self::IfThenElse {
            cond: Box::new(cond),
            then_expr: Box::new(then_expr),
            else_expr: Box::new(else_expr),
        }
    }

    pub fn apply(func: Expr, arg: Expr) -> Self {
        Self::Apply {
            func: Box::new(func),
            arg: Box::new(arg),
        }
    }

    pub fn app(mut func: Expr, args: impl IntoIterator<Item = Expr>) -> Self {
        for arg in args {
            func = Expr::apply(func, arg);
        }
        func
    }

    pub fn bin(op: BinOp, lhs: Expr, rhs: Expr) -> Self {
        Self::BinOp {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }

    pub fn un(op: UnOp, expr: Expr) -> Self {
        Self::UnOp {
            op,
            expr: Box::new(expr),
        }
    }

    pub fn render(&self) -> String {
        let mut p = Printer::new();
        p.expr(self, Prec::Top);
        p.finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    Top = 0,
    LetIfLambda = 10,
    Or = 20,
    And = 30,
    Cmp = 40,
    Add = 50,
    Mul = 60,
    Apply = 70,
    Select = 80,
    Atom = 90,
}

impl Expr {
    fn prec(&self) -> Prec {
        match self {
            Expr::LetIn { .. } | Expr::IfThenElse { .. } | Expr::Lambda { .. } => Prec::LetIfLambda,
            Expr::BinOp { op, .. } => match op {
                BinOp::Or => Prec::Or,
                BinOp::And => Prec::And,
                BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Prec::Cmp,
                BinOp::Add | BinOp::Sub | BinOp::Update => Prec::Add,
                BinOp::Mul | BinOp::Div | BinOp::Mod => Prec::Mul,
            },
            Expr::Apply { .. } => Prec::Apply,
            Expr::Select { .. } => Prec::Select,
            Expr::UnOp { .. } => Prec::Mul,
            Expr::Ident(_)
            | Expr::Int(_)
            | Expr::Bool(_)
            | Expr::Str(_)
            | Expr::Null
            | Expr::List(_)
            | Expr::AttrSet { .. } => Prec::Atom,
        }
    }
}

struct Printer {
    out: String,
    indent: usize,
}

impl Printer {
    fn new() -> Self {
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

    fn expr(&mut self, e: &Expr, parent: Prec) {
        let my_prec = e.prec();
        let need_parens = my_prec < parent;

        self.parens_if(need_parens, |p| match e {
            Expr::Ident(s) => p.push(s),
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
                        p.expr(item, Prec::Top);
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
        assert!(s.contains("{"));
        assert!(s.contains("x = 1;"));
    }
}
