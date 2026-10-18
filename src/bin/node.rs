//! The `node` command: the Node-compatible personality of the toolchain.
//!
//! Installed as `node`, this runs scripts the way Node does. It is the same code
//! as `codets node`, so both spellings behave identically.

use std::process::ExitCode;

fn main() -> ExitCode {
    codets::cli::main_as(codets::cli::Personality::Node)
}
