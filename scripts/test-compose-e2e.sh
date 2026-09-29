#!/bin/sh
# Boot the development Compose stack and verify both published services and
# the console-to-host proxy. This is intentionally separate from the fast,
# Docker-free launcher test because it builds images and starts containers.
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "${SCRIPT_DIR}/.." && pwd)
RUN_ID=${E2E_RUN_ID:-$(date +%s)-$$}
RUN_ID=$(printf '%s' "$RUN_ID" | tr -cd 'a-zA-Z0-9_-')
PROJECT="opencompany-compose-e2e-${RUN_ID}"
OPENCOMPANY_PORT=${E2E_API_PORT:-0}
CONSOLE_PORT=${E2E_CONSOLE_PORT:-0}
OPENCOMPANY_COMPANY=${E2E_COMPANY:-marketing_agency}
OPENCOMPANY_ADMIN_EMAIL=${E2E_ADMIN_EMAIL:-e2e-admin@example.com}
cookie_jar=
export OPENCOMPANY_PORT CONSOLE_PORT OPENCOMPANY_COMPANY OPENCOMPANY_ADMIN_EMAIL

compose() {
    docker compose \
        --project-directory "${REPO_ROOT}/deploy" \
        --project-name "$PROJECT" \
        --file "${REPO_ROOT}/deploy/docker-compose.yml" \
        --file "${REPO_ROOT}/deploy/docker-compose.dev.yml" \
        "$@"
}

cleanup() {
    status=$?
    trap - 0 HUP INT TERM
    rm -f "$cookie_jar"
    if [ "$status" -ne 0 ]; then
        compose ps >&2 || true
        compose logs --no-color >&2 || true
    fi
    # Keep the named Cargo/npm caches: the normal launcher does the same, and
    # a second smoke run should not rebuild the world. `down -v` remains the
    # explicit way to remove them.
    compose down --volumes --remove-orphans >/dev/null 2>&1 || true
    exit "$status"
}
trap cleanup 0 HUP INT TERM

wait_for_url() {
    label=$1
    url=$2
    attempts=0
    until curl --connect-timeout 2 --max-time 10 \
        --fail --silent --output /dev/null "$url"; do
        attempts=$((attempts + 1))
        if [ "$attempts" -ge 120 ]; then
            echo "compose e2e: ${label} did not become ready at ${url}" >&2
            return 1
        fi
        sleep 1
    done
}

command -v docker >/dev/null 2>&1 || {
    echo "compose e2e: docker is required" >&2
    exit 127
}
command -v curl >/dev/null 2>&1 || {
    echo "compose e2e: curl is required" >&2
    exit 127
}

echo "compose e2e: starting API on :${OPENCOMPANY_PORT} and console on :${CONSOLE_PORT}"
printf '%s\n%s\n' "${E2E_ADMIN_PASSWORD:-e2e-password}" \
    "${E2E_ADMIN_PASSWORD:-e2e-password}" \
    | OPENCOMPANY_PROJECT_NAME="$PROJECT" \
        "${SCRIPT_DIR}/init-demo-admin.sh" "$OPENCOMPANY_COMPANY" "$OPENCOMPANY_ADMIN_EMAIL"
compose up --build --detach

OPENCOMPANY_PORT=$(compose port opencompany 8080 | sed 's/.*://')
CONSOLE_PORT=$(compose port console 80 | sed 's/.*://')
api_url="http://localhost:${OPENCOMPANY_PORT}"
console_url="http://localhost:${CONSOLE_PORT}"
wait_for_url "API" "${api_url}/healthz"
wait_for_url "console" "${console_url}/"

api_health=$(curl --connect-timeout 2 --max-time 10 \
    --fail --silent --show-error "${api_url}/healthz")
proxied_health=$(curl --connect-timeout 2 --max-time 10 \
    --fail --silent --show-error "${console_url}/healthz")
if [ "$api_health" != "$proxied_health" ]; then
    echo "compose e2e: console /healthz did not return the API response" >&2
    exit 1
fi

if ! curl --connect-timeout 2 --max-time 10 \
    --fail --silent --show-error "${console_url}/" \
    | grep -F '<title>OpenCompany Console</title>' >/dev/null; then
    echo "compose e2e: console did not serve the Vite application" >&2
    exit 1
fi

company_id=$(awk '
    /^\[company\][[:space:]]*$/ { in_company = 1; next }
    /^\[/ { in_company = 0 }
    in_company && /^[[:space:]]*name[[:space:]]*=/ {
        sub(/^[^=]*=[[:space:]]*"/, "")
        sub(/"[[:space:]]*$/, "")
        print
        exit
    }
' "${REPO_ROOT}/companies/${OPENCOMPANY_COMPANY}/company.toml" \
    | tr '[:upper:]' '[:lower:]' \
    | sed 's/[^a-z0-9][^a-z0-9]*/-/g; s/^-//; s/-$//')
cookie_jar=$(mktemp)
login_payload=$(printf '{"email":"%s","password":"%s"}' \
    "$OPENCOMPANY_ADMIN_EMAIL" "${E2E_ADMIN_PASSWORD:-e2e-password}")
curl --connect-timeout 2 --max-time 10 --fail --silent --show-error \
    --cookie-jar "$cookie_jar" --cookie "$cookie_jar" \
    -H 'content-type: application/json' \
    -d "$login_payload" \
    "${api_url}/api/v1/companies/${company_id}/auth/login" >/dev/null
curl --connect-timeout 2 --max-time 10 --fail --silent --show-error \
    --cookie "$cookie_jar" \
    "${api_url}/api/v1/companies/${company_id}/auth/me" >/dev/null

health_start_period=$(compose config | awk '/start_period:/ { print $2; exit }')
case "$health_start_period" in
    5m|6m|7m|8m|9m|10m|11m|12m|[1-9][0-9]m|[1-9]h|[1-9][0-9]h) ;;
    *) echo "compose e2e: healthcheck start_period is less than 5m: ${health_start_period:-missing}" >&2; exit 1 ;;
esac

echo "compose e2e passed: API ${api_url}, console ${console_url}, proxy connected"
