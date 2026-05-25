#![no_std]
#![allow(unsafe_code)]
extern crate alloc;

use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use core::{cell::UnsafeCell, fmt::Write};

use saphyr::{LoadableYamlNode, Scalar, Yaml};
use serde_json::{Map, Number, Value};

struct OutputCell(UnsafeCell<Vec<u8>>);

unsafe impl Sync for OutputCell {}

static LAST_OUTPUT: OutputCell = OutputCell(UnsafeCell::new(Vec::new()));

#[no_mangle]
pub extern "C" fn yaml_to_json(ptr: u32, len: u32) -> u32 {
    let input = unsafe { alloc::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let result = str::from_utf8(input)
        .map_err(|err| alloc::format!("input is not UTF-8: {err}"))
        .and_then(yaml_to_json_string);

    match result {
        Ok(json) => {
            set_last_output(json.into_bytes());
            0
        }
        Err(err) => {
            set_last_output(err.into_bytes());
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn alloc(len: u32) -> *mut u8 {
    let box_ptr = alloc::boxed::Box::new(Vec::<u8>::with_capacity(len as usize));
    let ptr = Box::leak(box_ptr);
    ptr.as_mut_ptr()
}

#[no_mangle]
pub extern "C" fn last_output_ptr() -> u32 {
    unsafe { (&*LAST_OUTPUT.0.get()).as_ptr() as u32 }
}

#[no_mangle]
pub extern "C" fn last_output_len() -> u32 {
    unsafe { (&*LAST_OUTPUT.0.get()).len() as u32 }
}

fn set_last_output(bytes: Vec<u8>) {
    unsafe {
        *LAST_OUTPUT.0.get() = bytes;
    }
}

fn yaml_to_json_string(input: &str) -> Result<String, String> {
    let docs = Yaml::load_from_str(input).map_err(|err| err.to_string())?;
    let doc = docs
        .first()
        .ok_or_else(|| "input did not contain a YAML document".to_string())?;

    let value = yaml_to_json_value(doc)?;
    let json = serde_json::to_string(&value).map_err(|err| err.to_string())?;
    Ok(json_to_ascii(&json))
}

fn yaml_to_json_value(node: &Yaml<'_>) -> Result<Value, String> {
    match node {
        Yaml::Value(scalar) => scalar_to_json_value(scalar),
        Yaml::Sequence(values) => values
            .iter()
            .map(yaml_to_json_value)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Yaml::Mapping(mapping) => {
            let mut entries = mapping
                .iter()
                .map(|(key, value)| {
                    Ok((
                        key.as_str()
                            .ok_or("mapping key is not a string")?
                            .to_string(),
                        value,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            entries.sort_by(|left, right| left.0.cmp(&right.0));

            let mut out = Map::new();
            for (key, value) in entries {
                out.insert(key, yaml_to_json_value(value)?);
            }
            Ok(Value::Object(out))
        }
        Yaml::Tagged(_, value) => yaml_to_json_value(value),
        Yaml::Representation(..) => {
            let mut parsed = node.clone();
            if !parsed.parse_representation_recursive() {
                return Err("failed to resolve YAML scalar representation".to_string());
            }
            yaml_to_json_value(&parsed)
        }
        Yaml::Alias(_) => Err("YAML aliases are not supported by this example".to_string()),
        Yaml::BadValue => Err("YAML document contains an invalid value".to_string()),
    }
}

fn scalar_to_json_value(scalar: &Scalar<'_>) -> Result<Value, String> {
    match scalar {
        Scalar::Null => Ok(Value::Null),
        Scalar::Boolean(value) => Ok(Value::Bool(*value)),
        Scalar::Integer(value) => Ok(Value::Number(Number::from(*value))),
        Scalar::FloatingPoint(value) => {
            let value = value.into_inner();
            if !value.is_finite() {
                return Err("non-finite YAML floats cannot be represented as JSON".to_string());
            }
            Number::from_f64(value)
                .map(Value::Number)
                .ok_or_else(|| "YAML float cannot be represented as a JSON number".to_string())
        }
        Scalar::String(value) => Ok(Value::String(value.to_string())),
    }
}

fn json_to_ascii(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for ch in json.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            push_json_unicode_escape(ch, &mut out);
        }
    }
    out
}

fn push_json_unicode_escape(ch: char, out: &mut String) {
    let code = ch as u32;
    if code <= 0xFFFF {
        write!(out, "\\u{code:04x}").expect("writing to String cannot fail");
        return;
    }

    let code = code - 0x10000;
    let high = 0xD800 + (code >> 10);
    let low = 0xDC00 + (code & 0x3FF);
    write!(out, "\\u{high:04x}\\u{low:04x}").expect("writing to String cannot fail");
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
            yaml_to_json_string("message: hello λ\n").unwrap(),
            r#"{"message":"hello \u03bb"}"#
        );
    }
}
