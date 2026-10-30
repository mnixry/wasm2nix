use pretty::BoxDoc;

use crate::AttrKey;

pub(super) type Doc = BoxDoc<'static, ()>;

pub(super) fn attr_key(key: &AttrKey) -> Doc {
    match key {
        AttrKey::Ident(s) => text(s.clone()),
        AttrKey::Str(s) => string_doc(s),
    }
}

pub(super) fn text(s: impl Into<String>) -> Doc {
    Doc::text(s.into())
}

pub(super) fn parens(doc: Doc) -> Doc {
    text("(").append(doc).append(")")
}

pub(super) fn string_doc(s: &str) -> Doc {
    text(format!("\"{}\"", escape_string(s)))
}

/// Joins `docs` with `sep`, pairing neighbours level by level so the result
/// is a balanced tree: a left-leaning chain of 50k binds would overflow the
/// stack when rendered.
pub(super) fn join(docs: impl IntoIterator<Item = Doc>, sep: &Doc) -> Doc {
    let mut docs = docs.into_iter().collect::<Vec<_>>();
    while docs.len() > 1 {
        let mut next = Vec::with_capacity(docs.len().div_ceil(2));
        let mut pairs = docs.into_iter();
        while let Some(left) = pairs.next() {
            if let Some(right) = pairs.next() {
                next.push(left.append(sep.clone()).append(right));
            } else {
                next.push(left);
            }
        }
        docs = next;
    }

    docs.pop().unwrap_or_else(Doc::nil)
}

pub(super) fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // `${` would start an interpolation inside a Nix string.
            '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
            _ => out.push(c),
        }
    }
    out
}
