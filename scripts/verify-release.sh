#!/bin/sh
# Verifies a published release by using it.
#
# A release is not done when the workflow is green; it is done when its assets have
# been downloaded, checked against the published checksum file and installed. This
# downloads every asset the checksum file lists, verifies each digest, lists each
# archive to confirm it holds the binary and the documents, and installs through
# install.sh into a throwaway store that is removed on the way out.
#
# It needs the network and is not part of `cargo test`; the offline contract lives in
# tests/release_contract.rs. The PowerShell script of the same name covers
# install.ps1, so running both covers both installers.
#
#   scripts/verify-release.sh                     the newest tagged version
#   scripts/verify-release.sh --version=0.3.0     a specific release

set -eu

cd "$(dirname "$0")/.."

version="${CODETS_VERSION:-}"

for argument in "$@"; do
    case "$argument" in
        --version=*) version="${argument#--version=}" ;;
        -h | --help)
            sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            printf 'verify-release: unknown argument %s\n' "$argument" >&2
            exit 2
            ;;
    esac
done

if [ -z "$version" ]; then
    version="$(git tag --list 'v*' --sort=-v:refname | head -n 1 | sed 's/^v//')"
fi
[ -n "$version" ] || { printf 'verify-release: no v* tag to verify\n' >&2; exit 1; }

step() { printf '\n== %s\n' "$1"; }
fail() {
    printf 'verify-release: %s\n' "$1" >&2
    exit 1
}

# The digest of a file, on whichever of the three tools this machine has. macOS does
# not ship `sha256sum`, and the release workflow packages with the same selection.
digest_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        openssl dgst -sha256 "$1" | sed 's/.*= *//'
    fi
}

base="https://github.com/neko233-com/codets/releases/download"
checksums="codets-${version}-checksums.txt"

work="$(mktemp -d 2>/dev/null || mktemp -d -t codets-verify-release)"
store="$(mktemp -d 2>/dev/null || mktemp -d -t codets-verify-release-store)"
trap 'rm -rf "$work" "$store"' EXIT INT TERM

# A Windows binary needs a Windows-shaped CODETS_HOME; everywhere else the path
# already is one.
store_home="$(cygpath -w "$store" 2>/dev/null || echo "$store")"

step "v${version}: the published checksum file"
curl -fsSL -o "$work/$checksums" "$base/v${version}/$checksums" ||
    fail "cannot download $base/v${version}/$checksums"
sed 's/^/  /' "$work/$checksums"

step "every asset it lists, and its digest"
assets="$(awk '{print $2}' "$work/$checksums")"
[ -n "$assets" ] || fail "the checksum file lists nothing"
count=0
for asset in $assets; do
    curl -fsSL -o "$work/$asset" "$base/v${version}/$asset" ||
        fail "cannot download $asset"
    expected="$(awk -v name="$asset" '$2 == name {print $1}' "$work/$checksums")"
    actual="$(digest_of "$work/$asset")"
    [ "$expected" = "$actual" ] ||
        fail "$asset failed its checksum: expected $expected, got $actual"

    # What an installer needs is inside: the binary for that platform, and the
    # licence and readme the archive carries alongside it.
    case "$asset" in
        *windows*) inner="codets.exe" ;;
        *) inner="codets" ;;
    esac
    tar -tzf "$work/$asset" | grep -q "/$inner\$" || fail "$asset has no $inner"
    tar -tzf "$work/$asset" | grep -q "/README.md\$" || fail "$asset has no README.md"
    tar -tzf "$work/$asset" | grep -q "/LICENSE\$" || fail "$asset has no LICENSE"
    printf 'ok   %-46s %s...\n' "$asset" "${actual:0:16}"
    count=$((count + 1))
done

step "the asset set is the five published platforms"
expected_set="codets-${version}-aarch64-apple-darwin.tar.gz
codets-${version}-aarch64-pc-windows-msvc.tar.gz
codets-${version}-aarch64-unknown-linux-gnu.tar.gz
codets-${version}-x86_64-pc-windows-msvc.tar.gz
codets-${version}-x86_64-unknown-linux-gnu.tar.gz"
# The same list as release::published_triples(); tests/release_contract.rs keeps the
# workflow honest, and this keeps the published files honest.
if [ "$(printf '%s\n' $assets | sort)" != "$(printf '%s\n' "$expected_set" | sort)" ]; then
    printf 'verify-release: the published assets are not the five platforms:\n' >&2
    printf '%s\n' $assets | sort | sed 's/^/  /' >&2
    exit 1
fi
printf 'ok   %s assets, macOS is Apple Silicon only\n' "$count"

step "install.sh, against the real release"
# The version is pinned so the anonymous releases API is not needed: a shared egress
# address can exhaust that quota on its own.
CODETS_HOME="$store_home" CODETS_VERSION="$version" sh install.sh --level 22 ||
    fail "install.sh could not install v${version}"

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN* | Windows_NT) exe=".exe" ;;
    *) exe="" ;;
esac
for command in codets node npm; do
    [ -f "$store/bin/$command$exe" ] || fail "install.sh left no bin/$command$exe"
done

reported="$(CODETS_HOME="$store_home" "$store/bin/codets$exe" --version)"
case "$reported" in
    "codets $version "*) printf 'ok   the installed binary reports: %s\n' "$reported" ;;
    *) fail "the installed binary reports \`$reported\`, not codets $version" ;;
esac
level="$(CODETS_HOME="$store_home" "$store/bin/node$exe" -p process.version)"
[ "$level" = "v22.12.0" ] || fail "node reports $level, not v22.12.0 (the level the installer was given)"

printf '\nverify-release: v%s — %s assets verified, install.sh installed and ran\n' "$version" "$count"
