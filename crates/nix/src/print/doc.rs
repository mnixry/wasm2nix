use pretty::BoxDoc;

use crate::{AttrBind, AttrKey};

pub(super) type Doc = BoxDoc<'static, ()>;

pub(super) fn attr_key(key: &AttrKey) -> Doc {
    match key {
        AttrKey::Ident(s) => text(s.clone()),
        AttrKey::Str(s) => string_doc(s),
    }
}

pub(super) fn attr_path(bind: &AttrBind) -> Doc {
    join(bind.path.iter().map(attr_key), &text("."))
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

pub(super) fn join(docs: impl IntoIterator<Item = Doc>, sep: &Doc) -> Doc {
    let mut docs = docs.into_iter().collect::<Vec<_>>();
    if docs.is_empty() {
        return Doc::nil();
    }

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

    docs.pop().expect("non-empty docs")
}

pub(super) fn escape_string(s: &str) -> String {
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
