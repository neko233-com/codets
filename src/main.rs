//! The `codets` toolchain command line.
//!
//! This binary is the toolchain itself: it runs TypeScript and JavaScript,
//! manages the installed Node compatibility levels, and can stand in for `node`
//! and `npm` when it is reached through one of their shims.

use std::process::ExitCode;

fn main() -> ExitCode {
    codets::cli::main()
}
