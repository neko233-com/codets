#!/bin/sh
# Finds the scratch directories a test run leaves in the temp directory.
#
# Every test and script in this repository puts its scratch state in the temp
# directory under a `codets-` prefix, and a full test run must leave none of it
# behind. This is the check `scripts/pre-release` runs, and the check to run after
# `cargo test`.
#
# Run it when no tests are running: a test that is still in flight looks exactly
# like a leak.
#
#   scripts/check-temp-clean.sh              report, exit 1 if anything was found
#   scripts/check-temp-clean.sh --fix        remove what was found
#   scripts/check-temp-clean.sh --temp-root=/somewhere

set -eu

temp_root="${TMPDIR:-/tmp}"
fix=0

for argument in "$@"; do
    case "$argument" in
        --fix) fix=1 ;;
        --temp-root=*) temp_root="${argument#--temp-root=}" ;;
        -h | --help)
            sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            printf 'check-temp-clean: unknown argument %s\n' "$argument" >&2
            exit 2
            ;;
    esac
done

if [ ! -d "$temp_root" ]; then
    printf 'check-temp-clean: %s does not exist\n' "$temp_root" >&2
    exit 2
fi

found="$(find "$temp_root" -maxdepth 1 -name 'codets-*' -type d 2>/dev/null | sort || true)"
if [ -z "$found" ]; then
    printf 'check-temp-clean: clean, no codets-* directory in %s\n' "$temp_root"
    exit 0
fi

count="$(printf '%s\n' "$found" | wc -l | tr -d ' ')"
printf 'check-temp-clean: found %s leftover director%s in %s\n' \
    "$count" "$([ "$count" -eq 1 ] && echo y || echo ies)" "$temp_root"
printf '%s\n' "$found" | sed 's/^/  /'

if [ "$fix" -ne 1 ]; then
    printf 'check-temp-clean: run with --fix to remove them\n'
    exit 1
fi

printf '%s\n' "$found" | while IFS= read -r path; do
    rm -rf "$path"
done

remaining="$(find "$temp_root" -maxdepth 1 -name 'codets-*' -type d 2>/dev/null | sort || true)"
if [ -n "$remaining" ]; then
    printf 'check-temp-clean: some of them survived; a running test may still hold them open\n' >&2
    printf '%s\n' "$remaining" | sed 's/^/  /' >&2
    exit 1
fi
printf 'check-temp-clean: removed %s\n' "$count"
