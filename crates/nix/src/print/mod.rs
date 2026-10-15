//! Pretty-printing of [`Expr`] trees to Nix source.

mod doc;
mod prec;
#[cfg(test)]
mod tests;

use doc::{Doc, attr_key, attr_path, join, parens, string_doc, text};
use prec::{Assoc, ChildSide, Prec};

use crate::{AttrBind, BinOp, Expr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    pub width: usize,
    pub indent: usize,
    pub style: RenderStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStyle {
    Pretty,
    Compact,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            width: 100,
            indent: 2,
            style: RenderStyle::Pretty,
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
        let width = match self.options.style {
            RenderStyle::Pretty => self.options.width,
            RenderStyle::Compact => usize::MAX,
        };
        doc.render(width, &mut out)
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
            Expr::Assert { conds, body } => {
                let asserts = conds.iter().map(|cond| {
                    text("assert ")
                        .append(self.expr_prec(cond, Prec::Top))
                        .append(";")
                        .group()
                });
                join(asserts, &Doc::line())
                    .append(Doc::line())
                    .append(self.expr_prec(body, Prec::Top))
                    .group()
            }
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
