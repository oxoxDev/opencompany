#!/bin/sh
# Focused tests for first-admin Compose initialization without starting Docker.
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
TMP_DIR=$(mktemp -d)
DOCKER_LOG="${TMP_DIR}/docker.log"
export DOCKER_LOG
TEST_PROJECT="opencompany-init-demo-test-$$"
trap 'rm -rf "$TMP_DIR"' EXIT HUP INT TERM

cat >"${TMP_DIR}/docker" <<'EOF'
#!/bin/sh
set -eu
case "$*" in
    volume\ inspect\ *) exit 1 ;;
    volume\ create\ *) printf '%s\n' "$*" >>"$DOCKER_LOG"; exit 0 ;;
esac
if [ "${FAIL_COMPOSE_STEP:-}" = up ]; then
    case "$*" in *" up "*) exit 42 ;; esac
fi
if [ "${FAIL_COMPOSE_STEP:-}" = stop ]; then
    case "$*" in *" stop "*) exit 43 ;; esac
fi
printf 'admin=%s\n' "$OPENCOMPANY_ADMIN_EMAIL"
printf 'company=%s\n' "$OPENCOMPANY_COMPANY"
case "$*" in
    *"--file docker-compose.yml"*"--file docker-compose.dev.yml"*) ;;
    *) echo "unexpected compose files: $*" >&2; exit 1 ;;
esac
printf 'args=%s\n' "$*"
case "$*" in
    *"run --rm --no-deps -T opencompany"*) cat ;;
    *" up --build --detach --wait --wait-timeout 120 opencompany"*) ;;
    *" stop console opencompany"*) ;;
    *) echo "unexpected compose command: $*" >&2; exit 1 ;;
esac
EOF
chmod +x "${TMP_DIR}/docker"

output=$(printf 'correct horse\ncorrect horse\n' \
    | PATH="${TMP_DIR}:$PATH" OPENCOMPANY_PROJECT_NAME="$TEST_PROJECT" \
        "${SCRIPT_DIR}/init-demo-admin.sh" \
        marketing admin@example.com)
printf '%s\n' "$output" | grep -F 'admin=admin@example.com' >/dev/null
for cache_volume in \
    opencompany-cargo-registry \
    opencompany-cargo-git \
    opencompany-cargo-target \
    opencompany-frontend-node-modules; do
    grep -F "volume create ${cache_volume}" "$DOCKER_LOG" >/dev/null
done
printf '%s\n' "$output" | grep -F 'up --build --detach --wait --wait-timeout 120 opencompany' >/dev/null
printf '%s\n' "$output" | grep -F 'stop console opencompany' >/dev/null
printf '%s\n' "$output" | grep -F -- '--company agentic-marketing-agency' >/dev/null
printf '%s\n' "$output" | grep -F -- '--email admin@example.com' >/dev/null
printf '%s\n' "$output" | grep -F -- '--no-change-required --home /data' >/dev/null
printf '%s\n' "$output" | grep -F 'administrator initialized' >/dev/null

if printf 'one\ntwo\n' | PATH="${TMP_DIR}:$PATH" \
    OPENCOMPANY_PROJECT_NAME="$TEST_PROJECT" \
    "${SCRIPT_DIR}/init-demo-admin.sh" marketing admin@example.com >/dev/null 2>&1; then
    echo "init-demo-admin test: mismatched passwords unexpectedly succeeded" >&2
    exit 1
fi

if printf 'one\none\n' | PATH="${TMP_DIR}:$PATH" \
    OPENCOMPANY_PROJECT_NAME="$TEST_PROJECT" \
    "${SCRIPT_DIR}/init-demo-admin.sh" marketing not-an-email >/dev/null 2>&1; then
    echo "init-demo-admin test: invalid email unexpectedly succeeded" >&2
    exit 1
fi

if printf 'one\none\n' | PATH="${TMP_DIR}:$PATH" FAIL_COMPOSE_STEP=up \
    OPENCOMPANY_PROJECT_NAME="$TEST_PROJECT" \
    "${SCRIPT_DIR}/init-demo-admin.sh" marketing admin@example.com >/dev/null 2>&1; then
    echo "init-demo-admin test: Compose readiness failure unexpectedly succeeded" >&2
    exit 1
fi

if printf 'one\none\n' | PATH="${TMP_DIR}:$PATH" FAIL_COMPOSE_STEP=stop \
    OPENCOMPANY_PROJECT_NAME="$TEST_PROJECT" \
    "${SCRIPT_DIR}/init-demo-admin.sh" marketing admin@example.com >/dev/null 2>&1; then
    echo "init-demo-admin test: Compose stop failure unexpectedly succeeded" >&2
    exit 1
fi

echo "init-demo-admin tests passed"
