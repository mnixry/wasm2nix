//! Runs the WebAssembly spec's WAST scripts against generated Nix.

mod error;
mod eval;
mod expect;
mod script;
mod state;

pub use error::Error;
pub use error::HarnessResolverError;
pub use error::Result;
pub use script::run_wast_script;
