#!/bin/sh
# One-line installer for codets.
#
#   curl -fsSL https://raw.githubusercontent.com/neko233-com/codets/main/install.sh | sh
#
# It downloads the published archive for this platform, checks it against the
# release's checksum file, unpacks the binary and hands it to `codets bootstrap`,
# which owns the store layout — so this script never needs to know how a version
# directory is arranged.
#
# Arguments are passed to `bootstrap`:
#
#   ... | sh -s -- --level 22      # claim Node 22 for the installed toolchain
#   ... | sh -s -- --no-default    # do not make it this machine's default
#
# Environment:
#
#   CODETS_VERSION=<v>        install a specific release instead of the newest
#   CODETS_HOME=<path>        where the toolchain store lives (~/.codets)
#   CODETS_DOWNLOAD_BASE=<url>  asset base URL, for a mirror or a file:// copy
#   CODETS_API_BASE=<url>     release API base URL, for a mirror
#
# The asset names it reconstructs are a contract with `src/release.rs`:
#
#   <base>/v<version>/codets-<version>-<triple>.tar.gz
#   <base>/v<version>/codets-<version>-checksums.txt

set -eu

# `--help` is answered before anything else: it has to work on a machine with no
# curl and no release to install, which is exactly when someone reads it.
for argument in "$@"; do
    case "$argument" in
        -h | --help)
            sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
    esac
done

REPO="neko233-com/codets"

say() {
    printf 'codets-install: %s\n' "$*"
}

fail() {
    printf 'codets-install: %s\n' "$*" >&2
    exit 1
}

# Pick one command per job, so a missing tool is a clear message rather than a
# half-finished install.
CURL=""
WGET=""
if command -v curl >/dev/null 2>&1; then
    CURL="curl"
elif command -v wget >/dev/null 2>&1; then
    WGET="wget"
else
    fail "neither curl nor wget is available; install one of them and retry"
fi

if command -v sha256sum >/dev/null 2>&1; then
    DIGEST="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
    DIGEST="shasum"
elif command -v openssl >/dev/null 2>&1; then
    DIGEST="openssl"
else
    fail "no sha256 tool (sha256sum, shasum or openssl) is available; a download that cannot be verified will not be installed"
fi

command -v tar >/dev/null 2>&1 || fail "tar is not available"

fetch_to() {
    if [ -n "$CURL" ]; then
        "$CURL" -fsSL "$1" -o "$2"
    else
        "$WGET" -qO "$2" "$1"
    fi
}

fetch_text() {
    if [ -n "$CURL" ]; then
        "$CURL" -fsSL "$1"
    else
        "$WGET" -qO- "$1"
    fi
}

sha256_of() {
    case "$DIGEST" in
        sha256sum) "$DIGEST" "$1" | cut -d' ' -f1 ;;
        shasum) "$DIGEST" -a 256 "$1" | cut -d' ' -f1 ;;
        openssl) "$DIGEST" dgst -sha256 "$1" | sed 's/.*= *//' ;;
    esac
}

# The target triple, spelled the way the release assets spell it.
detect_triple() {
    os="$(uname -s)"
    machine="$(uname -m)"
    case "$os" in
        Linux)
            case "$machine" in
                x86_64 | amd64) echo "x86_64-unknown-linux-gnu" ;;
                aarch64 | arm64) echo "aarch64-unknown-linux-gnu" ;;
                *) fail "no published build for Linux on $machine" ;;
            esac
            ;;
        Darwin)
            case "$machine" in
                arm64 | aarch64) echo "aarch64-apple-darwin" ;;
                *) fail "no published build for macOS on $machine; codets publishes for Apple Silicon only" ;;
            esac
            ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT)
            case "$machine" in
                x86_64 | amd64) echo "x86_64-pc-windows-msvc" ;;
                aarch64 | arm64) echo "aarch64-pc-windows-msvc" ;;
                *) fail "no published build for Windows on $machine" ;;
            esac
            ;;
        *) fail "no published build for $os" ;;
    esac
}

latest_version() {
    api="${CODETS_API_BASE:-https://api.github.com/repos/${REPO}}"
    fetch_text "${api%/}/releases/latest" |
        tr ',' '\n' |
        sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' |
        head -n 1
}

version="${CODETS_VERSION:-}"
if [ -z "$version" ]; then
    if [ -n "${CODETS_DOWNLOAD_BASE:-}" ] && [ -z "${CODETS_API_BASE:-}" ]; then
        fail "CODETS_DOWNLOAD_BASE is set, so the newest version cannot be looked up; set CODETS_VERSION as well"
    fi
    version="$(latest_version)"
    # An anonymous caller of the releases API is rate-limited hard — a shared egress
    # address can exhaust the quota on its own — so the message names the way out.
    [ -n "$version" ] ||
        fail "cannot determine the newest release from the releases API (it may be rate-limiting an anonymous caller); set CODETS_VERSION to install a specific release"
fi
version="${version#v}"

triple="$(detect_triple)"
base="${CODETS_DOWNLOAD_BASE:-https://github.com/${REPO}/releases/download}"
base="${base%/}"
release_url="${base}/v${version}"
asset="codets-${version}-${triple}.tar.gz"

say "installing codets ${version} for ${triple}"

work="$(mktemp -d 2>/dev/null || mktemp -d -t codets-install)"
trap 'rm -rf "$work"' EXIT INT TERM

say "downloading ${asset}"
fetch_to "${release_url}/${asset}" "${work}/${asset}" ||
    fail "cannot download ${release_url}/${asset}"

# A missing checksum file is tolerated, the way `codets upgrade` tolerates it; a
# mismatch never is.
if fetch_to "${release_url}/codets-${version}-checksums.txt" "${work}/checksums.txt" 2>/dev/null; then
    expected="$(tr -d '\r' < "${work}/checksums.txt" | sed -n "s/^\([0-9a-fA-F]*\) *\**${asset}\$/\1/p" | head -n 1)"
    if [ -z "$expected" ]; then
        say "warning: the checksum file does not list ${asset}; installing unverified"
    else
        actual="$(sha256_of "${work}/${asset}")"
        if [ "$actual" != "$expected" ]; then
            fail "${asset} failed its checksum: expected sha256 ${expected}, got ${actual}"
        fi
        say "checksum matches"
    fi
else
    say "warning: no checksum file for ${version} to check the download against"
fi

tar -xzf "${work}/${asset}" -C "$work" || fail "cannot unpack ${asset}"

binary_name="codets"
case "$triple" in
    *windows*) binary_name="codets.exe" ;;
esac

binary="$(find "$work" -type f -name "$binary_name" -print | head -n 1)"
[ -n "$binary" ] || fail "${asset} does not contain ${binary_name}"
chmod +x "$binary" 2>/dev/null || true

"$binary" bootstrap "$@"

say "add the bin directory to PATH:"
"$binary" env
