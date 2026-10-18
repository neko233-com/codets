#!/bin/sh
# Installs the Vite 8 demo with codets and runs its TypeScript.
#
# This is the end-to-end check against the real npm registry: it drives `codets npm
# install` in examples/vite8-demo, asserts what the install produced, and then runs
# the demo's own TypeScript with `codets node` — no bundler and no build step.
#
# It needs the network and is therefore not part of `cargo test`; run it before a
# release alongside the other online checks (`cargo test -- --ignored` with
# CODETS_LIVE_REGISTRY=1). It is also the slower half of the pair: the Vite 8 tree is
# large.
#
# What it deliberately does not do is run Vite's own command line. See the last step
# of its output and the demo's README for why.
#
#   scripts/verify-demo.sh                    uses target/release, then target/debug
#   scripts/verify-demo.sh --jobs 4           fewer download threads
#   CODETS_BINARY=/path/to/codets scripts/verify-demo.sh

set -eu

cd "$(dirname "$0")/.."

jobs=8
binary="${CODETS_BINARY:-}"

for argument in "$@"; do
    case "$argument" in
        --jobs=*) jobs="${argument#--jobs=}" ;;
        --binary=*) binary="${argument#--binary=}" ;;
        -h | --help)
            sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            printf 'verify-demo: unknown argument %s\n' "$argument" >&2
            exit 2
            ;;
    esac
done

step() { printf '\n== %s\n' "$1"; }
fail() {
    printf 'verify-demo: %s\n' "$1" >&2
    exit 1
}

demo="$(pwd)/examples/vite8-demo"
[ -f "$demo/package.json" ] || fail "the demo is missing from $demo"

step "0/6 the toolchain"
if [ -z "$binary" ]; then
    for candidate in target/release/codets target/debug/codets; do
        if [ -x "$candidate" ]; then
            binary="$(pwd)/$candidate"
            break
        fi
    done
fi
[ -n "$binary" ] || fail 'no codets binary found; run `cargo build --release` first, or set CODETS_BINARY'
printf 'using %s\n' "$binary"

step "1/6 a clean starting point"
# Both of these are generated, so the run starts from the same place every time.
rm -rf "$demo/node_modules" "$demo/package-lock.json"

work="$(mktemp -d 2>/dev/null || mktemp -d -t codets-verify-demo)"
trap 'rm -rf "$work"' EXIT INT TERM

cd "$demo"

step "2/6 which registry this will use"
"$binary" npm config get registry --origin || fail 'the toolchain could not report its registry'

step "3/6 npm install"
set +e
"$binary" npm install --jobs "$jobs" > "$work/out" 2> "$work/err"
code=$?
set -e
cat "$work/out"
warnings="$(grep -c '^npm: warning:' "$work/err" || true)"
printf 'warnings: %s (not asserted empty: a tree this size has ranges no flat installer can satisfy)\n' "$warnings"
grep '^npm: warning:' "$work/err" | head -n 5 | sed 's/^/  /' || true
[ "$code" -eq 0 ] || fail "npm install exited with $code"

step "4/6 what the install produced"
[ -f node_modules/vite/package.json ] || fail 'node_modules/vite is missing'
vite="$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' node_modules/vite/package.json | head -n 1)"
case "$vite" in
    8.*) ;;
    *) fail "vite $vite is not the 8.x line the demo pins" ;;
esac
[ -f package-lock.json ] || fail 'no package-lock.json was written'
packages="$(grep -c '"node_modules/' package-lock.json || true)"
[ "$packages" -gt 0 ] || fail 'the lockfile lists no packages'
[ -f node_modules/.bin/vite ] || fail 'node_modules/.bin has no vite shim'
printf 'vite %s, %s packages in the lockfile, vite shim present\n' "$vite" "$packages"

step "5/6 the demo's own TypeScript"
printed="$("$binary" node src/summary.ts)" || fail 'running src/summary.ts failed'
printf '%s\n' "$printed"
for line in 'vite@8' 'codets@0.3' 'vite + codets: 2 releases described'; do
    case "$printed" in
        *"$line"*) ;;
        *) fail "the program's output is missing \"$line\"" ;;
    esac
done

step "6/6 what this did not verify"
printf 'Not run, and not runnable with this toolchain: `vite dev`, `vite build`, `vite preview`.\n'
printf "  Vite's CLI spawns esbuild through child_process, which codets does not implement,\n"
printf '  and loads Rolldown and lightningcss as native .node modules, which it refuses.\n'
printf '  So this verifies installation and TypeScript, not the Vite command line.\n'

printf '\nverify-demo: ok\n'
