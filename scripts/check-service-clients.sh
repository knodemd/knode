#!/bin/sh
# Checks that the client ids and secrets of the services Knode's connectors sign in to are in the
# environment, so a build embeds them, and optionally that a built binary carries them.
#
# Usage: check-service-clients.sh [--require] [--binary FILE]...
#
#   --require      fail when one is missing or malformed (release builds of a stable version);
#                  without it each problem is a warning and the check passes.
#   --binary FILE  also check that FILE embeds every value that is set; repeatable.
#
# Values are never printed. In GitHub Actions the problems are annotations. The variables, and
# where their values come from, are described in the handbook (07-releasing.md).
set -eu

require=0
binaries=""
while [ $# -gt 0 ]; do
    case "$1" in
        --require) require=1 ;;
        --binary)
            [ $# -ge 2 ] || { echo "error: --binary needs a file" >&2; exit 2; }
            binaries="$binaries
$2"
            shift
            ;;
        *) echo "error: unknown argument $1" >&2; exit 2 ;;
    esac
    shift
done

problems=0
report() {
    problems=$((problems + 1))
    if [ "$require" = 1 ]; then level=error; else level=warning; fi
    if [ "${GITHUB_ACTIONS:-}" = true ]; then
        echo "::$level::$1"
    else
        echo "$level: $1" >&2
    fi
}

# name | connector it is for
required="KNODE_GOOGLE_CLIENT_ID|Gmail (Sign in with Google)
KNODE_GOOGLE_CLIENT_SECRET|Gmail (Sign in with Google)
KNODE_TELEGRAM_API_ID|Telegram
KNODE_TELEGRAM_API_HASH|Telegram
KNODE_SLACK_CLIENT_ID|Slack"

value_of() {
    eval "printf '%s' \"\${$1:-}\"" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//'
}

old_ifs="$IFS"
IFS='
'
for entry in $required; do
    name="${entry%%|*}"
    connector="${entry#*|}"
    value="$(value_of "$name")"
    if [ -z "$value" ]; then
        report "$name is not set: this build has no complete $connector client, so the connector needs it at run time"
        continue
    fi
    case "$name" in
        KNODE_GOOGLE_CLIENT_ID)
            case "$value" in
                *.apps.googleusercontent.com) ;;
                *) report "$name does not end in .apps.googleusercontent.com" ;;
            esac
            ;;
        KNODE_TELEGRAM_API_ID)
            case "$value" in
                ''|*[!0-9]*|0) report "$name is not a positive number" ;;
            esac
            ;;
        KNODE_TELEGRAM_API_HASH)
            if ! printf '%s' "$value" | grep -Eq '^[0-9a-f]{32}$'; then
                report "$name is not 32 hexadecimal characters"
            fi
            ;;
        KNODE_SLACK_CLIENT_ID)
            if ! printf '%s' "$value" | grep -Eq '^[0-9]+\.[0-9]+$'; then
                report "$name does not look like a Slack client id (digits.digits)"
            fi
            ;;
    esac
done

limits="$(value_of KNODE_SLACK_LIMITS)"
case "$limits" in
    ''|marketplace|Marketplace) ;;
    *) report "KNODE_SLACK_LIMITS is '$limits'; only 'marketplace' (or nothing) means anything" ;;
esac

for binary in $binaries; do
    if [ ! -f "$binary" ]; then
        report "$binary does not exist"
        continue
    fi
    for entry in $required; do
        name="${entry%%|*}"
        value="$(value_of "$name")"
        [ -n "$value" ] || continue
        if ! LC_ALL=C grep -aqF -- "$value" "$binary"; then
            report "$binary does not embed $name, though it was set when checking"
        fi
    done
done
IFS="$old_ifs"

if [ "$problems" -gt 0 ] && [ "$require" = 1 ]; then
    exit 1
fi
if [ "$problems" = 0 ]; then
    echo "Service clients: all set"
fi
