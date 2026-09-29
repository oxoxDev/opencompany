#!/usr/bin/env bash
# Upload a Cargo release directory (executables plus dSYM/DWP/PDB companions)
# to the shared OpenCompany Rust project. Credentials arrive only through the
# environment and are never printed.
set -euo pipefail

artifacts=${1:?usage: upload-sentry-symbols.sh <cargo-release-directory>}
cli=${SENTRY_CLI:-sentry-cli}

for variable in SENTRY_AUTH_TOKEN SENTRY_ORG SENTRY_PROJECT SENTRY_URL; do
  if [ -z "${!variable:-}" ]; then
    echo "missing required environment variable: ${variable}" >&2
    exit 1
  fi
done

if [ ! -e "$artifacts" ]; then
  echo "Rust artifact path does not exist: $artifacts" >&2
  exit 1
fi

"$cli" debug-files upload \
  --org "$SENTRY_ORG" \
  --project "$SENTRY_PROJECT" \
  --include-sources \
  --wait-for 60 \
  "$artifacts"
