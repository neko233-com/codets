#!/bin/sh
# The local gate a release has to pass before its tag is pushed.
#
# GitHub Actions minutes are limited and the release matrix runs exactly once, on
# a tag, so everything that can be checked locally is checked here first:
#
#   1. the working tree is clean, because a release is built from a commit
#   2. `cargo test`, offline
#   3. `cargo build --release --locked`, the command the workflow runs
#   4. nothing the test suite left behind in the temp directory
#   5. the version in Cargo.toml has a CHANGELOG section, which is what becomes the
#      release notes
#
# The online checks are deliberately not here: `cargo test -- --ignored` with
# CODETS_LIVE_REGISTRY=1, and scripts/verify-demo.sh. Both need the network and are
# run by hand.
#
# The script only reads the tree and writes to target/, so running it twice is the
# same as running it once.
#
#   scripts/pre-release.sh                     the gate
#   scripts/pre-release.sh --allow-dirty       ...while the tree still has edits
#   scripts/pre-release.sh --skip-changelog    a dry run before the CHANGELOG entry
#                                              exists; never for a real tag

set -eu

cd "$(dirname "$0")/.."

allow_dirty=0
skip_changelog=0

for argument in "$@"; do
    case "$argument" in
        --allow-dirty) allow_dirty=1 ;;
        --skip-changelog) skip_changelog=1 ;;
        -h | --help)
            sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            printf 'pre-release: unknown argument %s\n' "$argument" >&2
            exit 2
            ;;
    esac
done

step() { printf '\n== %s\n' "$1"; }
fail() {
    printf 'pre-release: %s\n' "$1" >&2
    exit 1
}

# `cargo` is not on PATH when the rustup proxy scripts are missing, which happens
# on a machine where only the toolchains are installed.
find_cargo() {
    if command -v cargo >/dev/null 2>&1; then
        command -v cargo
        return
    fi
    for candidate in "$HOME/.cargo/bin/cargo" "$HOME"/.rustup/toolchains/*/bin/cargo; do
        if [ -x "$candidate" ]; then
            printf '%s\n' "$candidate"
            return
        fi
    done
}

cargo="$(find_cargo)"
[ -n "$cargo" ] || fail "cannot find cargo; see AGENTS.md for this machine's toolchain layout"
echo "pre-release: using $cargo"

step "1/5 the working tree"
if [ "$allow_dirty" -eq 0 ]; then
    dirty="$(git status --porcelain)"
    if [ -n "$dirty" ]; then
        printf '%s\n' "$dirty" | sed 's/^/  /'
        fail "the tree has uncommitted changes; commit them, or pass --allow-dirty"
    fi
    echo "clean"
else
    echo "skipped (--allow-dirty)"
fi

step "2/5 cargo test"
"$cargo" test --quiet || fail "the test suite failed"

step "3/5 cargo build --release --locked"
"$cargo" build --release --locked ||
    fail "the release build failed; on Windows, check the LIB note in AGENTS.md"

step "4/5 temp leftovers"
sh scripts/check-temp-clean.sh ||
    fail "the test run left scratch behind; run scripts/check-temp-clean.sh --fix once nothing else is running"

step "5/5 version and changelog"
version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1)"
[ -n "$version" ] || fail "cannot read the version from Cargo.toml"
# The newest section is the one that describes this release, so it has to be this
# version's. An `[Unreleased]` section above it means the tree is not ready — and a
# version whose section is no longer newest has already shipped, which is the other
# way re-tagging goes wrong.
newest="$(grep -m 1 '^## ' CHANGELOG.md || true)"
# Keep a Changelog writes `## [0.3.0] - 2026-10-19`, so the version is what is inside
# the brackets and the rest of the line is not part of the comparison.
newest_version="$(printf '%s' "$newest" | sed -n 's/^## \[\([^]]*\)\].*/\1/p')"
if [ "$newest_version" = "$version" ]; then
    echo "Cargo.toml says $version, and CHANGELOG.md's newest section is [$newest_version]"
elif [ "$skip_changelog" -eq 1 ]; then
    echo "warning: CHANGELOG.md's newest section is \"$newest\", not [$version] (--skip-changelog)"
else
    fail "CHANGELOG.md's newest section is \"$newest\" but Cargo.toml says $version. Turn [Unreleased] into [$version], or bump the version: the release notes come from that section."
fi

printf '\npre-release: ready.\n'
printf '  the tag has to point at a commit on the default branch, and CI checks that.\n'
printf '  next:  git tag v%s && git push origin v%s\n' "$version" "$version"
