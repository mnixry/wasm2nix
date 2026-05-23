use snafu::prelude::*;
use std::path::PathBuf;

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Failed to read Wasm input from {}", path.display()))]
    ReadWasm {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("Failed to write Nix output to {}", path.display()))]
    WriteNix {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display("Wasm translation failed: {message}"))]
    TranslateWasm { message: String },

    #[snafu(display("{message}"))]
    Message { message: String },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
