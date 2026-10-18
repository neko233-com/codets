//! The `npm` command: the package-management personality of the toolchain.
//!
//! Installed as `npm`, this resolves, downloads, verifies and unpacks packages.
//! The transfer phase is multi-threaded; JavaScript execution stays
//! single-threaded, as the language requires.

use std::process::ExitCode;

fn main() -> ExitCode {
    codets::cli::main_as(codets::cli::Personality::Npm)
}
