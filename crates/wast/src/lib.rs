//! WAST specification test harness package.

pub mod error;
mod support_core;
mod support_eval;
mod support_state;

pub use error::{Error, Result};
pub use support_core::{WastHarness, WastHarnessOptions, run_wast_file_path, run_wast_script};

#[must_use]
pub const fn harness_nix() -> &'static str {
    include_str!("../nix/harness.nix")
}

#[must_use]
pub fn harness_nix_path() -> &'static std::path::Path {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/nix/harness.nix"))
}
