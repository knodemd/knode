# shellcheck shell=sh
# Loads the client ids and secrets of the services Knode's connectors sign in to (Google,
# Telegram, Slack) from core/.env.services into the environment, so `cargo build` embeds them
# through option_env!. Source it; do not run it:
#
#     . scripts/load-service-clients.sh && cargo build
#
# core/.env.services is for developer builds and is never committed (see
# core/.env.services.example). A variable already set and not empty in the environment wins over
# the file, so release builds take theirs from GitHub Actions secrets. Only the names below are
# read; any other line is ignored, and the file is parsed, never run.
#
# The connectors also read the same variables when Knode Service starts, which override the
# built-in values. The handbook (07-releasing.md) says where each value comes from.

# The file is KNODE_SERVICES_FILE when set (the Mac build scripts set it), else core/.env.services
# of the checkout the current folder is in.
knode_services_file="${KNODE_SERVICES_FILE:-}"
if [ -z "$knode_services_file" ]; then
    knode_services_file="$(git rev-parse --show-toplevel 2>/dev/null || echo .)/core/.env.services"
fi

if [ -f "$knode_services_file" ]; then
    while IFS= read -r knode_line || [ -n "$knode_line" ]; do
        case "$knode_line" in
            ''|'#'*) continue ;;
        esac
        knode_name="${knode_line%%=*}"
        knode_value="${knode_line#*=}"
        knode_name="$(printf '%s' "$knode_name" | sed 's/^export[[:space:]]*//; s/[[:space:]]*$//')"
        case "$knode_name" in
            KNODE_GOOGLE_CLIENT_ID|KNODE_GOOGLE_CLIENT_SECRET|KNODE_TELEGRAM_API_ID|\
            KNODE_TELEGRAM_API_HASH|KNODE_SLACK_CLIENT_ID|KNODE_SLACK_LIMITS) ;;
            *) continue ;;
        esac
        knode_value="$(printf '%s' "$knode_value" | sed "s/^[[:space:]]*//; s/[[:space:]]*\$//; s/^\"\\(.*\\)\"\$/\\1/; s/^'\\(.*\\)'\$/\\1/")"
        eval "knode_current=\${$knode_name:-}"
        if [ -z "$knode_current" ] && [ -n "$knode_value" ]; then
            export "$knode_name=$knode_value"
        fi
    done < "$knode_services_file"
fi
unset knode_services_file knode_line knode_name knode_value knode_current
