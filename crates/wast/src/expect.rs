use std::fmt;

use serde_json::Value as JsonValue;
use snafu::prelude::*;
use wast::{
    WastRet,
    core::{NanPattern, WastRetCore},
};

use crate::error::{ExpectedIntegerResultSnafu, Result, UnsupportedAssertReturnResultSnafu};

/// One expected `assert_return` result, in the JSON encoding the harness
/// returns from Nix.
#[derive(Debug)]
pub enum Expected {
    Exact(JsonValue),
    CanonicalNan32,
    ArithmeticNan32,
    CanonicalNan64,
    ArithmeticNan64,
}

impl Expected {
    pub fn from_ret(ret: &WastRet<'_>) -> Result<Self> {
        let WastRet::Core(ret) = ret else {
            return UnsupportedAssertReturnResultSnafu.fail();
        };
        Ok(match ret {
            WastRetCore::I32(value) => Self::Exact(value.cast_unsigned().into()),
            WastRetCore::I64(value) => Self::Exact((*value).into()),
            WastRetCore::F32(NanPattern::Value(value)) => Self::Exact(value.bits.into()),
            WastRetCore::F64(NanPattern::Value(value)) => {
                Self::Exact(value.bits.cast_signed().into())
            }
            WastRetCore::F32(NanPattern::CanonicalNan) => Self::CanonicalNan32,
            WastRetCore::F32(NanPattern::ArithmeticNan) => Self::ArithmeticNan32,
            WastRetCore::F64(NanPattern::CanonicalNan) => Self::CanonicalNan64,
            WastRetCore::F64(NanPattern::ArithmeticNan) => Self::ArithmeticNan64,
            WastRetCore::RefExtern(Some(value)) => {
                Self::Exact(serde_json::json!({ "extern": value }))
            }
            WastRetCore::RefNull(_) => Self::Exact(0.into()),
            _ => return UnsupportedAssertReturnResultSnafu.fail(),
        })
    }

    fn matches(&self, actual: &JsonValue) -> bool {
        let bits = || json_number_to_u64(actual).ok();
        match self {
            Self::Exact(expected) => expected == actual,
            Self::CanonicalNan32 => bits().is_some_and(|bits| f32_canonical_nan(bits as u32)),
            Self::ArithmeticNan32 => bits().is_some_and(|bits| f32_arithmetic_nan(bits as u32)),
            Self::CanonicalNan64 => bits().is_some_and(f64_canonical_nan),
            Self::ArithmeticNan64 => bits().is_some_and(f64_arithmetic_nan),
        }
    }
}

impl fmt::Display for Expected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(value) => write!(f, "{value}"),
            Self::CanonicalNan32 | Self::CanonicalNan64 => f.write_str("nan:canonical"),
            Self::ArithmeticNan32 | Self::ArithmeticNan64 => f.write_str("nan:arithmetic"),
        }
    }
}

pub fn results_match(expected: &[Expected], actual: &JsonValue) -> bool {
    actual.as_array().is_some_and(|actual| {
        actual.len() == expected.len()
            && expected
                .iter()
                .zip(actual)
                .all(|(expected, actual)| expected.matches(actual))
    })
}

pub fn results_label(expected: &[Expected]) -> String {
    let items = expected.iter().map(ToString::to_string).collect::<Vec<_>>();
    format!("[{}]", items.join(","))
}

pub fn json_number_to_u64(value: &JsonValue) -> Result<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().map(i64::cast_unsigned))
        .context(ExpectedIntegerResultSnafu)
}

const fn f32_canonical_nan(bits: u32) -> bool {
    (bits & 0x7F80_0000) == 0x7F80_0000 && (bits & 0x007F_FFFF) == 0x0040_0000
}

const fn f32_arithmetic_nan(bits: u32) -> bool {
    (bits & 0x7F80_0000) == 0x7F80_0000 && (bits & 0x0040_0000) != 0
}

const fn f64_canonical_nan(bits: u64) -> bool {
    (bits & 0x7FF0_0000_0000_0000) == 0x7FF0_0000_0000_0000
        && (bits & 0x000F_FFFF_FFFF_FFFF) == 0x0008_0000_0000_0000
}

const fn f64_arithmetic_nan(bits: u64) -> bool {
    (bits & 0x7FF0_0000_0000_0000) == 0x7FF0_0000_0000_0000 && (bits & 0x0008_0000_0000_0000) != 0
}
