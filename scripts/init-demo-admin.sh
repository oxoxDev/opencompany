#!/bin/sh
# Initialize the first email-auth administrator for a local Compose demo.
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "${SCRIPT_DIR}/.." && pwd)

# shellcheck source=scripts/lib/demos.sh
. "${SCRIPT_DIR}/lib/demos.sh"

usage() {
    cat >&2 <<'EOF'
Usage: ./scripts/init-demo-admin.sh <site> <email>

The helper boots the backend to initialize its data, stops the stack before
updating filesystem-backed credentials, and prompts for the first password.
EOF
}

if [ "$#" -ne 2 ]; then
    usage
    exit 2
fi

requested_site=$1
admin_email=$2
case "$requested_site" in
    '' | *[!a-zA-Z0-9_-]*)
        echo "opencompany: invalid demo name '${requested_site}'" >&2
        exit 2
        ;;
esac
company=$(resolve_demo_company "$requested_site")
company_dir="${REPO_ROOT}/companies/${company}"
project=${OPENCOMPANY_PROJECT_NAME:-$(demo_project_name "$company")}

if [ ! -f "${company_dir}/company.toml" ]; then
    echo "opencompany: unknown demo '${requested_site}'" >&2
    exit 2
fi

case "$admin_email" in
    *@*.*) ;;
    *)
        echo "opencompany: '${admin_email}' is not an email address" >&2
        exit 2
        ;;
esac

if ! command -v docker >/dev/null 2>&1; then
    echo "opencompany: docker is required" >&2
    exit 127
fi

# Match `company_id_from_name`: lowercase ASCII, collapse non-alphanumerics to
# one dash, then trim the ends. Shipped demo names are ASCII.
company_name=$(awk '
    /^\[company\][[:space:]]*$/ { in_company = 1; next }
    /^\[/ { in_company = 0 }
    in_company && /^[[:space:]]*name[[:space:]]*=/ {
        sub(/^[^=]*=[[:space:]]*"/, "")
        sub(/"[[:space:]]*$/, "")
        print
        exit
    }
' "${company_dir}/company.toml")
company_id=$(printf '%s' "$company_name" \
    | tr '[:upper:]' '[:lower:]' \
    | sed 's/[^a-z0-9][^a-z0-9]*/-/g; s/^-//; s/-$//')

if [ -z "$company_id" ]; then
    echo "opencompany: could not derive the company id from ${company_dir}/company.toml" >&2
    exit 1
fi

compose() {
    (
        cd "${REPO_ROOT}/deploy"
        OPENCOMPANY_COMPANY="$company" docker compose \
            --project-name "$project" \
            --file docker-compose.yml \
            --file docker-compose.dev.yml \
            "$@"
    )
}

password_file=$(mktemp)
confirmation_file=$(mktemp)
chmod 600 "$password_file" "$confirmation_file"
tty_state=
cleanup() {
    if [ -n "$tty_state" ]; then
        stty "$tty_state" 2>/dev/null || :
    fi
    rm -f "$password_file" "$confirmation_file"
}
trap cleanup 0 HUP INT TERM

if [ -t 0 ]; then
    printf 'New password for %s: ' "$admin_email" >&2
    tty_state=$(stty -g)
    stty -echo
fi
IFS= read -r password
printf '%s\n' "$password" >"$password_file"
unset password
if [ -t 0 ]; then
    printf '\nConfirm password: ' >&2
fi
IFS= read -r confirmation
printf '%s\n' "$confirmation" >"$confirmation_file"
unset confirmation
if [ -t 0 ]; then
    stty "$tty_state"
    tty_state=
    printf '\n' >&2
fi

if [ ! -s "$password_file" ] || [ "$(sed -n '1p' "$password_file")" = "" ]; then
    echo "opencompany: password cannot be empty" >&2
    exit 2
fi
if ! cmp -s "$password_file" "$confirmation_file"; then
    echo "opencompany: passwords do not match" >&2
    exit 2
fi

export OPENCOMPANY_ADMIN_EMAIL="$admin_email"
echo "opencompany: initializing '${company}' before creating its administrator"
ensure_demo_cache_volumes
compose up --build --detach --wait --wait-timeout 120 opencompany
compose stop console opencompany

cat "$password_file" | compose run --rm --no-deps -T opencompany \
    cargo run -p opencompany-core --bin opencompany -- \
    issue-password \
    --company "$company_id" \
    --email "$admin_email" \
    --no-change-required \
    --home /data

echo "opencompany: administrator initialized; restart with:"
if [ -n "${OPENCOMPANY_PROJECT_NAME:-}" ]; then
    echo "  OPENCOMPANY_PROJECT_NAME=${project} ./scripts/launch-demo.sh ${requested_site} up"
else
    echo "  ./scripts/launch-demo.sh ${requested_site} up"
fi
