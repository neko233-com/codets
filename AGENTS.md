# Working on codets

Notes for anyone — human or agent — changing this repository. It is short on
purpose: the rules that matter, and where to look for the rest.

The project is a pure-Rust replacement for Node.js and npm. One binary wears three
hats (`codets`, `node`, `npm`), manages its own toolchains, and ships a package
manager. `README.md` describes the surface, `docs/ARCHITECTURE.md` the internals.

## Build and test

```bash
cargo test                          # the whole suite, offline
cargo build --release --locked      # exactly what the release workflow builds
```

Two environment quirks are worth knowing about on Windows:

* A Visual Studio installation that ships only the `onecore` toolset makes
  `link.exe` fail with `LNK1104: cannot open file 'msvcrt.lib'`. Point `LIB` at the
  MSVC `onecore\x64` folder plus the SDK's `ucrt\x64` and `um\x64` folders first.
* When the rustup proxy scripts are missing from `~/.cargo/bin`, `cargo` is not on
  `PATH`. Call the toolchain directly:
  `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe`. `cargo
  clippy` has the same problem — call `cargo-clippy.exe clippy --all-targets`
  instead.

`cargo clippy` is not part of the gate. It currently reports a pre-existing
deny-level `never_loop` in `src/builtins.rs` plus a backlog of style lints. Do not
let that stop a change, and do not treat the backlog as permission to add more.

## The release rule

**A release happens only when a tag is pushed.** `.github/workflows/release.yml`
triggers on `v*` tags and on nothing else, because Actions minutes are limited and
a half-verified release is worse than a late one. `workflow_dispatch` exists for a
dry run: it builds and uploads the artifacts but creates no release.

Therefore:

* Run `scripts/pre-release` (either shell) and let it pass **before** tagging.
  It runs the tests, the release build, the temp-leak check and the
  version/CHANGELOG consistency check, in that order.
* Push the tag only from a commit on the default branch. CI verifies that the tag
  is an ancestor of the default branch and that the tag matches `Cargo.toml`;
  both checks fail the run rather than publishing a mislabelled asset.
* Never upload, replace or delete a release asset by hand, and never move a tag to
  point somewhere else. If a release is wrong, cut a new patch version.
* The CHANGELOG section for that version *is* the release notes. No section, no
  release — the workflow needs one to describe what it published. An `[Unreleased]`
  section at the top means the tree is not ready: rename it to the version being
  tagged, and bump `Cargo.toml` with it. `scripts/pre-release` checks exactly that and
  refuses to pass until the newest section is the version in `Cargo.toml`.
* Do not tag to "see if the pipeline works". Exercise the same steps locally
  (`scripts/pre-release`, plus the installer test in `tests/install_scripts.rs`);
  the matrix jobs at GitHub are the last place a problem should be discovered.

## Tests

**Offline by default.** Every test must pass with no network access. Registries are
faked with the directory backend (`Registry::open` on a path), which is why the
installer takes a `Registry` instead of building one internally.

**Live tests are opt-in and never run in CI.** A test that needs the real registry
carries `#[ignore]` *and* checks an environment variable before doing anything:

```rust
#[test]
#[ignore = "needs the real npm registry: run with --ignored and CODETS_LIVE_REGISTRY=1"]
fn installs_a_real_package() {
    if std::env::var("CODETS_LIVE_REGISTRY").as_deref() != Ok("1") {
        eprintln!("skipping: set CODETS_LIVE_REGISTRY=1");
        return;
    }
    // ...
}
```

**Idempotency is asserted, not assumed.** Any command that writes state gets a test
that runs it twice and compares the observable result — file set, file contents,
lockfile bytes, store layout. Installing the same manifest twice must converge; if
it does not, that is a bug, not a flaky test.

**Temp directories are RAII.** Use `tests/support`'s `Scratch`. It removes its
directory on `Drop`, so a failing or panicking test leaves nothing behind, and its
name (`codets-test-<suite>-<pid>-<nano>`) is unique per test so parallel tests never
share state. Do not hand-roll `std::env::temp_dir().join(...)` and a trailing
`remove_dir_all` in a new test — a panic skips that line and leaks the directory.

**After a full test run, `scripts/check-temp-clean` must report nothing.** That is
how a leak gets noticed.

When adding a command or a flag, cover:

* the happy path, through the real binary (`tests/cli.rs` runs the executables, not
  the argument parser);
* the failure path — the exit code and the message, since scripts key off them;
* what it does twice, if it writes anything;
* what it does with input it cannot use, which must be a clear message rather than a
  panic.

## The release asset naming contract

Asset names are an interface with installed tools: `codets upgrade` and both
installers reconstruct them from the version and the target triple. `src/release.rs`
is the source of truth, and these five places must agree with it:

| Place | What it depends on |
|---|---|
| `src/release.rs` | defines the names |
| `.github/workflows/release.yml` | produces them |
| `install.sh` | downloads them |
| `install.ps1` | downloads them |
| `tests/release_contract.rs`, `tests/install_scripts.rs` | assert them end to end |

Rename an asset without updating all five and the break shows up as a failed
download for users, not as a failing test. Windows ships `.tar.gz` like every other
platform: it has had `tar.exe` since Windows 10, and `codets` unpacks its own
archives anyway.

## Supported platforms

A release publishes five triples: `x86_64-pc-windows-msvc`,
`aarch64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`
and `aarch64-apple-darwin`.

**Intel macOS is deliberately not published.** Apple has shipped no Intel Mac since
2020, and GitHub's last Intel runner image (`macos-13`) is so backlogged that a
release once queued for hours behind it — while every other platform, including the
cross-compiled Windows ARM64 build, finished in minutes — and had to be cancelled
unpublished. `codets upgrade` and both installers tell an Intel Mac that there is no
published build rather than fetching a binary it cannot run.

The list lives in `release::published_triples()` and in the workflow's matrix, and
`tests/release_contract.rs` compares the two, so changing one without the other fails
a test instead of a user's download.

## Code style

* Comments explain **why**, not what. The surrounding code sets the register: prose,
  full sentences, no marketing.
* Dependencies must be pure Rust. This project deliberately has no C toolchain
  requirement; a dependency that needs one is a design change, not an upgrade. New
  dependencies need a reason in the commit message.
* Match the local shape of a file rather than importing a habit from elsewhere.
* Be honest about limits in `README.md`'s *Limits* section. A documented gap is
  useful; an undocumented one is a bug report waiting to happen.

## .gitignore rules

* Ignore **generated output only**. Never ignore a source file, a lockfile the
  project owns, or a fixture — if something generated is needed to build, it is not
  generated output.
* Add the ignore entry in the same commit as the thing that generates it.
* Scratch directories for a new demo or tool go under an `examples/*` pattern
  rather than being listed one by one.

## Commits

* One concern per commit, with a subject that reads as an instruction
  ("Make install prune stale packages") and a body that explains why.
* English, matching the existing history.
* Do not mix a behaviour change with a reformatting pass.

## When you add a command, flag or script

Walk this list — it is the difference between "works here" and "works":

1. `src/**` — the implementation, plus unit tests for anything with a decision in it.
2. `tests/cli.rs` — the same thing through the real binary.
3. `README.md` — the command table and, if it changes behaviour, the *Limits* list.
4. `CHANGELOG.md` — under `[Unreleased]`, in the right category.
5. `AGENTS.md` — if it adds a rule, a gate or a contract.
6. If it is a script: both shells (`.sh` and `.ps1`), and an entry in
   `scripts/`' usage notes here.
