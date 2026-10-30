//! Converts YAML to JSON. Built for `wasm32-unknown-unknown` and called from
//! Nix through wasm2nix; see `default.nix`.

#![no_std]
#![expect(
    unsafe_code,
    reason = "Wasm exports exchange raw linear-memory pointers"
)]
extern crate alloc;

use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::fmt::Write;
use core::str::Utf8Error;

use saphyr::LoadableYamlNode;
use saphyr::Scalar;
use saphyr::ScanError;
use saphyr::Yaml;
use serde_json::Map;
use serde_json::Number;
use serde_json::Value;
use snafu::OptionExt;
use snafu::ResultExt;
use snafu::Snafu;
use snafu::ensure;

#[derive(Debug, Snafu)]
enum Error {
    #[snafu(display("input is not UTF-8: {source}"))]
    InputNotUtf8 { source: Utf8Error },

    #[snafu(display("{source}"))]
    ParseYaml { source: ScanError },

    #[snafu(display("input did not contain a YAML document"))]
    NoDocument,

    #[snafu(display("mapping key is not a string"))]
    NonStringKey,

    #[snafu(display("failed to resolve YAML scalar representation"))]
    UnresolvedScalar,

    #[snafu(display("YAML aliases are not supported by this example"))]
    Alias,

    #[snafu(display("YAML document contains an invalid value"))]
    BadValue,

    #[snafu(display("YAML float {value} cannot be represented as a JSON number"))]
    NonFiniteFloat { value: f64 },

    #[snafu(display("{source}"))]
    EncodeJson { source: serde_json::Error },
}

/// The output of the last call, read through `last_output_ptr` and
/// `last_output_len`.
struct OutputCell(UnsafeCell<Vec<u8>>);

// SAFETY: Wasm modules run on one thread.
unsafe impl Sync for OutputCell {}

static LAST_OUTPUT: OutputCell = OutputCell(UnsafeCell::new(Vec::new()));

/// Converts the YAML at `ptr..ptr + len` to JSON. Returns 0 with the JSON, or
/// 1 with an error message, as the last output.
///
/// # Safety
///
/// `ptr..ptr + len` must be initialized linear memory, such as a buffer from
/// [`alloc`].
#[no_mangle]
pub unsafe extern "C" fn yaml_to_json(ptr: u32, len: u32) -> u32 {
    // SAFETY: the caller passes initialized memory; Wasm addresses are usize.
    let input = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let result = str::from_utf8(input)
        .context(InputNotUtf8Snafu)
        .and_then(yaml_to_json_string);
    let (status, output) = match result {
        Ok(json) => (0, json),
        Err(err) => (1, err.to_string()),
    };
    // SAFETY: no reference into `LAST_OUTPUT` outlives an export call.
    unsafe { *LAST_OUTPUT.0.get() = output.into_bytes() };
    status
}

/// Leaks a buffer of `len` bytes for the caller to write input into.
#[no_mangle]
pub extern "C" fn alloc(len: u32) -> *mut u8 {
    Vec::<u8>::with_capacity(len as usize).leak().as_mut_ptr()
}

#[no_mangle]
pub extern "C" fn last_output_ptr() -> u32 {
    // SAFETY: no reference into `LAST_OUTPUT` outlives an export call.
    unsafe { (*LAST_OUTPUT.0.get()).as_ptr() as u32 }
}

#[no_mangle]
pub extern "C" fn last_output_len() -> u32 {
    // SAFETY: no reference into `LAST_OUTPUT` outlives an export call.
    unsafe { (*LAST_OUTPUT.0.get()).len() as u32 }
}

fn yaml_to_json_string(input: &str) -> Result<String, Error> {
    let docs = Yaml::load_from_str(input).context(ParseYamlSnafu)?;
    let doc = docs.first().context(NoDocumentSnafu)?;
    let json = serde_json::to_string(&yaml_to_json_value(doc)?).context(EncodeJsonSnafu)?;
    Ok(json_to_ascii(&json))
}

fn yaml_to_json_value(node: &Yaml<'_>) -> Result<Value, Error> {
    match node {
        Yaml::Value(scalar) => scalar_to_json_value(scalar),
        Yaml::Sequence(values) => values
            .iter()
            .map(yaml_to_json_value)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Yaml::Mapping(mapping) => {
            // `Map` is ordered by key.
            let mut out = Map::new();
            for (key, value) in mapping {
                let key = key.as_str().context(NonStringKeySnafu)?;
                out.insert(key.to_string(), yaml_to_json_value(value)?);
            }
            Ok(Value::Object(out))
        }
        Yaml::Tagged(_, value) => yaml_to_json_value(value),
        Yaml::Representation(..) => {
            let mut parsed = node.clone();
            ensure!(
                parsed.parse_representation_recursive(),
                UnresolvedScalarSnafu
            );
            yaml_to_json_value(&parsed)
        }
        Yaml::Alias(_) => AliasSnafu.fail(),
        Yaml::BadValue => BadValueSnafu.fail(),
    }
}

fn scalar_to_json_value(scalar: &Scalar<'_>) -> Result<Value, Error> {
    Ok(match scalar {
        Scalar::Null => Value::Null,
        Scalar::Boolean(value) => Value::Bool(*value),
        Scalar::Integer(value) => Value::Number(Number::from(*value)),
        Scalar::FloatingPoint(value) => {
            let value = value.into_inner();
            Value::Number(Number::from_f64(value).context(NonFiniteFloatSnafu { value })?)
        }
        Scalar::String(value) => Value::String(value.to_string()),
    })
}

/// Escapes every non-ASCII character as `\uXXXX` UTF-16 units, since Nix
/// strings are bytes.
fn json_to_ascii(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for ch in json.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            for unit in ch.encode_utf16(&mut [0; 2]) {
                write!(out, "\\u{unit:04x}").expect("writing to a String cannot fail");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::yaml_to_json_string;

    #[test]
    fn converts_nested_yaml_to_sorted_json() {
        let yaml = r#"
name: wasm2nix
enabled: true
items:
  - 1
  - "two"
nested:
  z: null
  a: "quote: \""
"#;

        assert_eq!(
            yaml_to_json_string(yaml).unwrap(),
            r#"{"enabled":true,"items":[1,"two"],"name":"wasm2nix","nested":{"a":"quote: \"","z":null}}"#
        );
    }

    #[test]
    fn escapes_non_ascii_as_json_unicode() {
        assert_eq!(
            yaml_to_json_string("message: hello λ 😀\n").unwrap(),
            r#"{"message":"hello \u03bb \ud83d\ude00"}"#
        );
    }
}
