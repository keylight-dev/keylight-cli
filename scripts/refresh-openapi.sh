#!/usr/bin/env bash
#
# Refresh the checked-in OpenAPI snapshot that tests/coverage.rs reads.
#
# The coverage test deliberately does not fetch at test time: a networked test
# is flaky, fails offline, and turns a server deploy into a surprise red build
# on an unrelated commit. Running this instead makes API drift arrive as a
# reviewable diff.
#
# Usage:  scripts/refresh-openapi.sh [base-url]

set -euo pipefail

BASE="${1:-https://api.keylight.dev}"
DEST="$(cd "$(dirname "$0")/.." && pwd)/tests/fixtures/openapi.json"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

echo "fetching ${BASE}/v1/openapi.json"
curl -fsS "${BASE}/v1/openapi.json" -o "$TMP"

# Fail before overwriting rather than committing a truncated or error-page
# snapshot — the coverage test would then "pass" against nothing.
python3 - "$TMP" <<'PY'
import json, sys
spec = json.load(open(sys.argv[1]))
paths = spec.get("paths") or {}
verbs = {"get", "post", "patch", "put", "delete"}
ops = sum(1 for p in paths.values() for m in p if m in verbs)
if not ops:
    sys.exit("refusing to write: fetched document has no operations")
print(f"  {len(paths)} paths, {ops} operations")
PY

if cmp -s "$TMP" "$DEST"; then
  echo "no change"
else
  cp "$TMP" "$DEST"
  echo "updated $DEST — review the diff, then update src/endpoints.rs if the surface changed"
fi
