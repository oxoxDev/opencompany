#!/usr/bin/env bash
#
# Boot the mock brain and a host serving `companies/hive_demo`, run
# `scripts/measure-coordination.mjs` against it, and tear both down. One
# command for the offline, deterministic measurement; pass `--live` (or set
# an inference credential and omit `--mock`) to measure a real model instead.
#
#   scripts/measure-coordination.sh --mock            # scripted mock brain
#   TINYHUMANS_API_KEY=… scripts/measure-coordination.sh   # a real model
#
# Env:
#   MEASURE_BIND       host address          (default 127.0.0.1:8180)
#   MEASURE_BRAIN_BIND mock brain address    (default 127.0.0.1:8199)
#   MEASURE_DATA_DIR   instance data root    (default target/measure/data; wiped
#                      each run ONLY while it stays inside target/measure)
#   MEASURE_BINARY     path to the binary    (default target/debug/opencompany,
#                      which must be built with --features openhuman,mcp)
#   MEASURE_COMPANY    company to serve      (default companies/hive_demo)
#   MEASURE_SECONDS    tail deadline         (default 600)
#
# Every other argument is passed to the Node script (`--json`, `--desk …`).

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"

bind="${MEASURE_BIND:-127.0.0.1:8180}"
brain_bind="${MEASURE_BRAIN_BIND:-127.0.0.1:8199}"
data_dir="${MEASURE_DATA_DIR:-$root/target/measure/data}"
binary="${MEASURE_BINARY:-$root/target/debug/opencompany}"
company="${MEASURE_COMPANY:-$root/companies/hive_demo}"
seconds="${MEASURE_SECONDS:-600}"

mock=0
pass=()
for arg in "$@"; do
  case "$arg" in
    --mock) mock=1 ;;
    --live) mock=0 ;;
    *) pass+=("$arg") ;;
  esac
done

if [[ ! -x "$binary" ]]; then
  cat >&2 <<MSG
[measure] No OpenCompany binary at $binary.
Build one with the harness in it first:

    cargo build --locked --features openhuman,mcp --bin opencompany

or point MEASURE_BINARY at one you keep elsewhere.
MSG
  exit 98
fi

# A fresh data root per run, deleted only inside the repository's own scratch
# area — the same rule `frontend/test/e2e/host.sh` follows, for the same
# reason: a mistyped or inherited path must never take a home directory with it.
mkdir -p "$data_dir"
data_dir="$(cd "$data_dir" && pwd -P)"
scratch="$root/target/measure"
mkdir -p "$scratch"
scratch="$(cd "$scratch" && pwd -P)"
if [[ "$data_dir" == "$scratch"/?* ]]; then
  rm -rf -- "$data_dir"
  mkdir -p "$data_dir"
else
  echo "[measure] data root is outside target/measure; reusing it as it stands." >&2
fi

pids=()
cleanup() {
  for pid in "${pids[@]:-}"; do
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT

wait_for() {
  local url="$1" name="$2" tries=0
  until curl -fsS "$url" >/dev/null 2>&1; do
    tries=$((tries + 1))
    if (( tries > 120 )); then
      echo "[measure] $name never answered at $url" >&2
      exit 97
    fi
    sleep 0.5
  done
}

host_env=(
  "OPENCOMPANY_DATA_DIR=$data_dir"
  "OPENCOMPANY_SKIP_ACTIVATION_GATE=1"
  "OPENCOMPANY_ADMIN_EMAIL=harness-e2e@tinyhumans.ai"
)

if (( mock )); then
  echo "[measure] starting the mock brain on $brain_bind" >&2
  node "$root/frontend/test/e2e/mock-brain.mjs" --bind "$brain_bind" &
  pids+=($!)
  wait_for "http://$brain_bind/healthz" "mock brain"
  # The model too: an environment-routed provider maps each agent's tier
  # through the company's `[inference].models` table, and `hive_demo` maps
  # none (its inference block is the managed brain's). Without an explicit
  # id every seat turn is refused with "No model is chosen for this company"
  # and the room closes `failed` with two synthetic completions. The mock
  # brain answers whatever id it is sent.
  host_env+=(
    "OPENCOMPANY_INFERENCE_KEY=mock-brain"
    "OPENCOMPANY_INFERENCE_URL=http://$brain_bind/v1"
    "OPENCOMPANY_INFERENCE_MODEL=mock-brain"
  )
else
  if [[ -z "${OPENCOMPANY_INFERENCE_KEY:-}${TINYHUMANS_API_KEY:-}" ]]; then
    echo "[measure] no OPENCOMPANY_INFERENCE_KEY or TINYHUMANS_API_KEY set; pass --mock or export a credential." >&2
    exit 96
  fi
  for name in OPENCOMPANY_INFERENCE_KEY OPENCOMPANY_INFERENCE_URL TINYHUMANS_API_KEY TINYHUMANS_API_URL OPENCOMPANY_JEV_URL; do
    if [[ -n "${!name+x}" ]]; then host_env+=("$name=${!name}"); fi
  done
fi

for name in HOME PATH TMPDIR TZ LANG LC_ALL RUST_LOG RUST_BACKTRACE; do
  if [[ -n "${!name+x}" ]]; then host_env+=("$name=${!name}"); fi
done

echo "[measure] serving $company on $bind (data: $data_dir)" >&2
(cd "$root" && exec env -i "${host_env[@]}" "$binary" serve --bind "$bind" --company "$company") &
pids+=($!)
wait_for "http://$bind/healthz" "host"

node_args=(--base "http://$bind" --seconds "$seconds")
if (( mock )); then node_args+=(--mock); fi
node "$root/scripts/measure-coordination.mjs" "${node_args[@]}" "${pass[@]}"
