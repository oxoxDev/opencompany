#!/usr/bin/env bash
#
# Boot a host serving the readable-replies fixture company against a STAGING
# model, run `run.mjs` against it, and tear the host down. Opt-in; not in CI.
# See README.md.
#
#   OPENCOMPANY_INFERENCE_URL=https://staging-api.tinyhumans.ai/openai/v1 \
#   OPENCOMPANY_INFERENCE_KEY=… OPENCOMPANY_INFERENCE_MODEL=… \
#     scripts/evals/readable_replies/run.sh [--samples 3] [--json]
#
# Env:
#   READABLE_BIND      host address       (default 127.0.0.1:8280)
#   READABLE_DATA_DIR  instance data root (default target/evals/readable/data;
#                      wiped each run ONLY while it stays inside target/evals)
#   READABLE_BINARY    path to the binary (default target/debug/opencompany,
#                      built with --features openhuman,mcp)
#
# Every other argument is passed to the Node script.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"

bind="${READABLE_BIND:-127.0.0.1:8280}"
data_dir="${READABLE_DATA_DIR:-$root/target/evals/readable/data}"
binary="${READABLE_BINARY:-$root/target/debug/opencompany}"
company="$here/company"

url="${OPENCOMPANY_INFERENCE_URL:-}"
if [[ -z "$url" || -z "${OPENCOMPANY_INFERENCE_KEY:-}" || -z "${OPENCOMPANY_INFERENCE_MODEL:-}" ]]; then
  echo "[readable] set OPENCOMPANY_INFERENCE_URL, OPENCOMPANY_INFERENCE_KEY and OPENCOMPANY_INFERENCE_MODEL for a staging model." >&2
  exit 96
fi
host="$(printf '%s' "$url" | sed -E 's#^[a-zA-Z][a-zA-Z0-9+.-]*://([^/@]*@)?([^/:]+).*#\2#')"
if [[ "$host" != staging*.tinyhumans.ai ]]; then
  echo "[readable] refusing $url: this eval runs only against a staging*.tinyhumans.ai host, got host $host." >&2
  exit 95
fi

if [[ ! -x "$binary" ]]; then
  echo "[readable] no binary at $binary; build it with: cargo build --locked --features openhuman,mcp --bin opencompany" >&2
  exit 98
fi

mkdir -p "$data_dir"
data_dir="$(cd "$data_dir" && pwd -P)"
scratch="$root/target/evals"
mkdir -p "$scratch"
scratch="$(cd "$scratch" && pwd -P)"
if [[ "$data_dir" == "$scratch"/?* ]]; then
  rm -rf -- "$data_dir"
  mkdir -p "$data_dir"
else
  echo "[readable] data root is outside target/evals; reusing it as it stands." >&2
fi

host_env=(
  "OPENCOMPANY_DATA_DIR=$data_dir"
  "OPENCOMPANY_SKIP_ACTIVATION_GATE=1"
  "OPENCOMPANY_ADMIN_EMAIL=harness-e2e@tinyhumans.ai"
  "OPENCOMPANY_INFERENCE_URL=$url"
  "OPENCOMPANY_INFERENCE_KEY=$OPENCOMPANY_INFERENCE_KEY"
  "OPENCOMPANY_INFERENCE_MODEL=$OPENCOMPANY_INFERENCE_MODEL"
)
for name in HOME PATH TMPDIR TZ LANG LC_ALL RUST_LOG RUST_BACKTRACE; do
  if [[ -n "${!name+x}" ]]; then host_env+=("$name=${!name}"); fi
done

if curl -fsS "http://$bind/healthz" >/dev/null 2>&1; then
  echo "[readable] something already answers at http://$bind; pick a free READABLE_BIND" >&2
  exit 97
fi

pid=""
cleanup() { [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true; }
trap cleanup EXIT

echo "[readable] serving the fixture company on $bind (data: $data_dir)" >&2
(cd "$root" && exec env -i "${host_env[@]}" "$binary" serve --bind "$bind" --company "$company") &
pid=$!

tries=0
until curl -fsS "http://$bind/healthz" >/dev/null 2>&1; do
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "[readable] host exited before it answered at http://$bind/healthz" >&2
    exit 97
  fi
  tries=$((tries + 1))
  if (( tries > 120 )); then
    echo "[readable] host never answered at http://$bind/healthz" >&2
    exit 97
  fi
  sleep 0.5
done

node "$here/run.mjs" --base "http://$bind" --company "$company" "$@"
