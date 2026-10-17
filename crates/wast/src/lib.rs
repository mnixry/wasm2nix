//! WAST specification test harness package.

pub mod error;
mod eval;
mod expect;
mod script;
mod state;

pub use error::{Error, Result};
pub use script::run_wast_script;
