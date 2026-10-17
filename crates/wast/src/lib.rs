//! WAST specification test harness package.

pub mod error;
mod eval;
mod script;
mod state;

pub use error::{Error, Result};
pub use script::{WastHarness, WastHarnessOptions, run_wast_script};

#[must_use]
pub const fn harness_nix() -> &'static str {
    include_str!("../nix/harness.nix")
}

#[must_use]
pub fn harness_nix_path() -> &'static std::path::Path {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/nix/harness.nix"))
}
