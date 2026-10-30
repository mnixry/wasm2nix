//! Pretty-printing of [`Expr`] trees to Nix source.

mod doc;
mod format;
mod prec;
#[cfg(test)]
mod tests;

use format::Formatter;
use prec::Prec;

use crate::Expr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    pub width: usize,
    pub indent: u8,
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

pub fn render(expr: &Expr, options: &RenderOptions) -> String {
    let width = match options.style {
        RenderStyle::Pretty => options.width,
        RenderStyle::Compact => usize::MAX,
    };
    let formatter = Formatter {
        indent: isize::from(options.indent),
    };
    formatter
        .expr_prec(expr, Prec::Top)
        .pretty(width)
        .to_string()
}
