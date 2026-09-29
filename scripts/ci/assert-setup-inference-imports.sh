#!/usr/bin/env bash
#
# Fail if the first-run wizard imports Search's provider dialogs.
#
# `frontend/src/inference/` and `frontend/src/search-providers/` both ship a
# file called `AddProviderDialog.tsx` and one called `ProviderConnectDialog.tsx`,
# and they are unrelated features: one connects a model provider, the other
# connects a web-search provider. Their props overlap enough that importing the
# wrong pair type-checks, renders, and builds — the wizard would simply be the
# wrong feature, and nothing but a reader would notice.
#
# A grep is cheaper than the argument. This is the one rule the slice's own plan
# calls "the single easiest mistake in this entire plan".
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1
SETUP=frontend/src/views/setup

hits=$(grep -rn "search-providers/\(AddProviderDialog\|ProviderConnectDialog\)" "$SETUP" 2>/dev/null || true)
if [ -n "$hits" ]; then
  echo "✗ the setup wizard imports Search's provider dialogs, not the model ones."
  echo "  Import from @/inference/ — @/search-providers/ is the web-search add flow."
  echo "$hits" | sed 's/^/    /'
  exit 1
fi

echo "✓ setup wizard imports the model provider dialogs, not Search's"
