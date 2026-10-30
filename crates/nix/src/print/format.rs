use super::doc::Doc;
use super::doc::attr_key;
use super::doc::join;
use super::doc::parens;
use super::doc::string_doc;
use super::doc::text;
use super::prec::Assoc;
use super::prec::ChildSide;
use super::prec::Prec;
use crate::AttrBind;
use crate::BinOp;
use crate::Expr;
use crate::LetBind;

pub(super) struct Formatter {
    pub(super) indent: isize,
}

impl Formatter {
    /// Formats `expr`, parenthesized when it binds looser than `parent`.
    pub(super) fn expr_prec(&self, expr: &Expr, parent: Prec) -> Doc {
        let doc = self.expr(expr);
        if Prec::of(expr) < parent {
            parens(doc)
        } else {
            doc
        }
    }

    fn expr(&self, expr: &Expr) -> Doc {
        match expr {
            Expr::Ident(s) | Expr::Path(s) => text(s.clone()),
            // Nix cannot lex `9223372036854775808`, the magnitude of
            // `i64::MIN`.
            Expr::Int(i64::MIN) => text("(0 - 9223372036854775807 - 1)"),
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
            } => self.if_then_else(cond, then_expr, else_expr),
            Expr::Assert { conds, body } => self.asserts(conds, body),
            Expr::Apply { func, args } => self.apply(func, args),
            Expr::BinOp { op, lhs, rhs } => self.binop(*op, lhs, rhs),
            Expr::UnOp { op, expr } => text(op.as_str()).append(self.expr_prec(expr, Prec::Prefix)),
        }
    }

    /// `open`, the `items` indented on their own lines when the group breaks,
    /// then `close`. Callers group the result.
    fn block(&self, open: &'static str, items: Doc, close: &'static str) -> Doc {
        text(open)
            .append(Doc::line().append(items).nest(self.indent))
            .append(Doc::line())
            .append(close)
    }

    fn list(&self, items: &[Expr]) -> Doc {
        if items.is_empty() {
            return text("[ ]");
        }
        let items = join(
            items.iter().map(|item| self.expr_prec(item, Prec::Select)),
            &Doc::line(),
        );
        self.block("[", items, "]").group()
    }

    fn attrset(&self, recursive: bool, binds: &[AttrBind]) -> Doc {
        if binds.is_empty() {
            return text(if recursive { "rec { }" } else { "{ }" });
        }
        let binds = join(
            binds.iter().map(|bind| {
                attr_key(&bind.key)
                    .append(" = ")
                    .append(self.expr_prec(&bind.value, Prec::Top))
                    .append(";")
            }),
            &Doc::line(),
        );
        self.block(if recursive { "rec {" } else { "{" }, binds, "}")
            .group()
    }

    fn let_in(&self, binds: &[LetBind], body: &Expr) -> Doc {
        // `Expr::let_in` never builds an empty `let`, but the variant is
        // public.
        if binds.is_empty() {
            return self.expr_prec(body, Prec::Top);
        }
        let binds = join(
            binds.iter().map(|bind| {
                text(bind.name.clone())
                    .append(" = ")
                    .append(self.expr_prec(&bind.value, Prec::Top))
                    .append(";")
            }),
            &Doc::line(),
        );
        self.block("let", binds, "in")
            .append(Doc::line())
            .append(self.expr_prec(body, Prec::Top))
            .group()
    }

    fn if_then_else(&self, cond: &Expr, then_expr: &Expr, else_expr: &Expr) -> Doc {
        text("if")
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
            .group()
    }

    fn asserts(&self, conds: &[Expr], body: &Expr) -> Doc {
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

    fn apply(&self, func: &Expr, args: &[Expr]) -> Doc {
        let args = join(
            args.iter().map(|arg| self.expr_prec(arg, Prec::Select)),
            &Doc::line(),
        );
        self.expr_prec(func, Prec::Apply)
            .append(Doc::line())
            .append(args)
            .group()
    }

    fn binop(&self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Doc {
        self.bin_child(lhs, op, ChildSide::Left)
            .append(Doc::line())
            .append(op.as_str())
            .append(" ")
            .append(self.bin_child(rhs, op, ChildSide::Right))
            .group()
    }

    /// Parenthesizes an operand of equal precedence on the side `op` does not
    /// associate towards.
    fn bin_child(&self, expr: &Expr, op: BinOp, side: ChildSide) -> Doc {
        let parent = Prec::of_binop(op);
        let equal_needs_parens = Prec::of(expr) == parent
            && match (side, Assoc::of_binop(op)) {
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
}
