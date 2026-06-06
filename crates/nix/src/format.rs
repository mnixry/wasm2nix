use pretty::BoxDoc;

use crate::{AttrBind, AttrKey, BinOp, Expr, RenderOptions};

type Doc = BoxDoc<'static, ()>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    Top         = 0,
    PipeBack    = 10,
    PipeForward = 20,
    Implies     = 30,
    Or          = 40,
    And         = 50,
    Equal       = 60,
    Compare     = 70,
    Update      = 80,
    Prefix      = 90,
    Add         = 100,
    Mul         = 110,
    Concat      = 120,
    Apply       = 130,
    Select      = 140,
    Atom        = 150,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Assoc {
    Left,
    Right,
    None,
}

impl From<BinOp> for Prec {
    fn from(val: BinOp) -> Self {
        match val {
            BinOp::PipeBackward => Self::PipeBack,
            BinOp::PipeForward => Self::PipeForward,
            BinOp::Implies => Self::Implies,
            BinOp::Or => Self::Or,
            BinOp::And => Self::And,
            BinOp::Eq | BinOp::Neq => Self::Equal,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Self::Compare,
            BinOp::Update => Self::Update,
            BinOp::Add | BinOp::Sub => Self::Add,
            BinOp::Mul | BinOp::Div | BinOp::Mod => Self::Mul,
            BinOp::Concat => Self::Concat,
        }
    }
}

impl From<BinOp> for Assoc {
    fn from(val: BinOp) -> Self {
        match val {
            BinOp::Update | BinOp::Concat | BinOp::PipeBackward | BinOp::Implies => Self::Right,
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Mod
            | BinOp::And
            | BinOp::Or
            | BinOp::PipeForward => Self::Left,
            BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Self::None,
        }
    }
}

impl From<&Expr> for Prec {
    fn from(val: &Expr) -> Self {
        match val {
            Expr::LetIn { .. } | Expr::IfThenElse { .. } | Expr::Lambda { .. } => Self::Top,
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

pub struct Printer<'a> {
    doc: Option<Doc>,
    options: &'a RenderOptions,
}

impl<'a> Printer<'a> {
    pub const fn new(options: &'a RenderOptions) -> Self {
        Self { doc: None, options }
    }

    pub fn finish(self) -> String {
        let doc = self.doc.unwrap_or_else(Doc::nil);
        let mut out = Vec::new();
        doc.render(self.options.width, &mut out)
            .expect("rendering to Vec cannot fail");
        String::from_utf8(out).expect("Nix formatter only emits UTF-8")
    }

    pub fn expr(&mut self, expr: &Expr) {
        let indent = isize::try_from(self.options.indent).expect("indent fits in isize");
        self.doc = Some(Formatter { indent }.expr_prec(expr, Prec::Top));
    }
}

struct Formatter {
    indent: isize,
}

fn attr_key(key: &AttrKey) -> Doc {
    match key {
        AttrKey::Ident(s) => text(s.clone()),
        AttrKey::Str(s) => string_doc(s),
    }
}

fn attr_path(bind: &AttrBind) -> Doc {
    join(bind.path.iter().map(attr_key), &text("."))
}

impl Formatter {
    fn expr_prec(&self, expr: &Expr, parent: Prec) -> Doc {
        let doc = self.expr_inner(expr);
        if Into::<Prec>::into(expr) < parent {
            parens(doc)
        } else {
            doc
        }
    }

    #[allow(clippy::too_many_lines)]
    fn expr_inner(&self, expr: &Expr) -> Doc {
        match expr {
            Expr::Ident(s) | Expr::Raw(s) | Expr::Path(s) => text(s.clone()),
            Expr::Int(v) if *v == i64::MIN => text("(0 - 9223372036854775807 - 1)"),
            Expr::Int(v) if *v < 0 => parens(text(v.to_string())),
            Expr::Int(v) => text(v.to_string()),
            Expr::Bool(true) => text("true"),
            Expr::Bool(false) => text("false"),
            Expr::Str(s) => string_doc(s),
            Expr::Null => text("null"),
            Expr::List(items) => self.list(items),
            Expr::AttrSet { recursive, binds } => self.attrset(*recursive, binds),
            Expr::Select { expr, path } => {
                let mut doc = self.expr_prec(expr, Prec::Select);
                for attr in path.iter() {
                    doc = doc.append(".").append(attr_key(attr));
                }
                doc
            }
            Expr::Lambda { param, body } => text(param.clone())
                .append(":")
                .append(Doc::line())
                .append(self.expr_prec(body, Prec::Top))
                .group(),
            Expr::LetIn { binds, body } => self.let_in(binds, body),
            Expr::IfThenElse {
                cond,
                then_expr,
                else_expr,
            } => text("if")
                .append(Doc::line())
                .append(self.expr_prec(cond, Prec::Top))
                .append(Doc::line())
                .append("then")
                .append(Doc::line())
                .append(self.expr_prec(then_expr, Prec::Top))
                .append(Doc::line())
                .append("else")
                .append(Doc::line())
                .append(self.expr_prec(else_expr, Prec::Top))
                .group(),
            Expr::Apply { func, args } => {
                let args = join(
                    args.iter().map(|arg| self.expr_prec(arg, Prec::Select)),
                    &Doc::line(),
                );
                self.expr_prec(func, Prec::Apply)
                    .append(Doc::line())
                    .append(args)
                    .group()
            }
            Expr::BinOp { op, lhs, rhs } => self.binop(*op, lhs, rhs),
            Expr::UnOp { op, expr } => text(op.as_str()).append(self.expr_prec(expr, Prec::Prefix)),
        }
    }

    fn let_in(&self, binds: &[crate::LetBind], body: &Expr) -> Doc {
        if binds.is_empty() {
            return self.expr_prec(body, Prec::Top);
        }

        let bind_docs = join(
            binds.iter().map(|bind| {
                text(bind.name.clone())
                    .append(" = ")
                    .append(self.expr_prec(&bind.value, Prec::Top))
                    .append(";")
            }),
            &Doc::line(),
        );

        text("let")
            .append(Doc::line().append(bind_docs).nest(self.indent))
            .append(Doc::line())
            .append("in")
            .append(Doc::line())
            .append(self.expr_prec(body, Prec::Top))
            .group()
    }

    fn binop(&self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Doc {
        let prec = op.into();
        self.bin_child(lhs, prec, ChildSide::Left, op.into())
            .append(Doc::line())
            .append(op.as_str())
            .append(" ")
            .append(self.bin_child(rhs, prec, ChildSide::Right, op.into()))
            .group()
    }

    fn bin_child(&self, expr: &Expr, parent: Prec, side: ChildSide, assoc: Assoc) -> Doc {
        let child: Prec = expr.into();
        let equal_needs_parens = child == parent
            && match (side, assoc) {
                (ChildSide::Left, Assoc::Right | Assoc::None)
                | (ChildSide::Right, Assoc::Left | Assoc::None) => true,
                (ChildSide::Left, Assoc::Left) | (ChildSide::Right, Assoc::Right) => false,
            };
        self.expr_prec(
            expr,
            if equal_needs_parens {
                Prec::Atom
            } else {
                parent
            },
        )
    }

    fn list(&self, items: &[Expr]) -> Doc {
        if items.is_empty() {
            return text("[ ]");
        }

        let items = join(
            items.iter().map(|item| self.expr_prec(item, Prec::Select)),
            &Doc::line(),
        );
        text("[")
            .append(Doc::line().append(items).nest(self.indent))
            .append(Doc::line())
            .append("]")
            .group()
    }

    fn attrset(&self, recursive: bool, binds: &[AttrBind]) -> Doc {
        let head = if recursive { "rec {" } else { "{" };
        if binds.is_empty() {
            return text(if recursive { "rec { }" } else { "{ }" });
        }

        let bind_docs = join(
            binds.iter().map(|bind| {
                attr_path(bind)
                    .append(" = ")
                    .append(self.expr_prec(&bind.value, Prec::Top))
                    .append(";")
            }),
            &Doc::line(),
        );

        text(head)
            .append(Doc::line().append(bind_docs).nest(self.indent))
            .append(Doc::line())
            .append("}")
            .group()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildSide {
    Left,
    Right,
}

fn text(s: impl Into<String>) -> Doc {
    Doc::text(s.into())
}

fn parens(doc: Doc) -> Doc {
    text("(").append(doc).append(")")
}

fn string_doc(s: &str) -> Doc {
    text(format!("\"{}\"", escape_string(s)))
}

fn join(docs: impl IntoIterator<Item = Doc>, sep: &Doc) -> Doc {
    let mut docs = docs.into_iter();
    let Some(first) = docs.next() else {
        return Doc::nil();
    };
    docs.fold(first, |acc, doc| acc.append(sep.clone()).append(doc))
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
    use crate::{AttrKey, Expr};

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

        assert_eq!(e.render(), "let x = 1; y = x + 2; in { x = x; y = y; }");
    }

    #[test]
    fn wraps_with_render_width() {
        let e = Expr::attrset(
            false,
            vec![
                (AttrKey::ident("alpha"), Expr::int(1)),
                (AttrKey::ident("beta"), Expr::int(2)),
            ],
        );

        assert_eq!(
            e.render_with(&RenderOptions {
                width: 10,
                indent: 2
            }),
            "{\n  alpha = 1;\n  beta = 2;\n}"
        );
    }

    #[test]
    fn preserves_left_associative_rhs_with_parentheses() {
        let e = Expr::bin(
            BinOp::Sub,
            Expr::ident("a"),
            Expr::bin(BinOp::Sub, Expr::ident("b"), Expr::ident("c")),
        );

        assert_eq!(e.render(), "a - (b - c)");
    }

    #[test]
    fn preserves_right_associative_lhs_with_parentheses() {
        let e = Expr::bin(
            BinOp::Update,
            Expr::bin(BinOp::Update, Expr::ident("a"), Expr::ident("b")),
            Expr::ident("c"),
        );

        assert_eq!(e.render(), "(a // b) // c");
    }

    #[test]
    fn flattens_apply_and_select_constructors() {
        let e = Expr::app(
            Expr::select(
                Expr::select(Expr::ident("builtins"), AttrKey::ident("elemAt")),
                AttrKey::ident("nested"),
            ),
            vec![Expr::ident("xs"), Expr::int(0)],
        );

        assert_eq!(e.render(), "builtins.elemAt.nested xs 0");
    }
}
