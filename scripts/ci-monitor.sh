#!/bin/bash
# CI Monitor — watches GitHub Actions workflow using gh run watch (efficient streaming).
# Usage: ./scripts/ci-monitor.sh [run_id]
#   run_id defaults to the latest run on the current branch.
# Exits 0 on success, 1 on failure, 2 on timeout.
# No polling — gh run watch uses GitHub's event-stream API.

set -euo pipefail

RUN_ID="${1:-}"
BRANCH=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "main")
TIMEOUT_SEC="${CI_MONITOR_TIMEOUT:-1200}"  # 20 min default

if [ -z "$RUN_ID" ]; then
  echo ":: CI Monitor: fetching latest run for branch '$BRANCH'..."
  RUN_ID=$(gh run list --branch "$BRANCH" --json databaseId --limit 1 --jq '.[0].databaseId' 2>/dev/null || echo "")
  if [ -z "$RUN_ID" ]; then
    echo "ERROR: No CI run found for branch '$BRANCH'"
    exit 2
  fi
  echo "   Latest run: $RUN_ID"
fi

echo ":: Watching CI run $RUN_ID on '$BRANCH' (timeout ${TIMEOUT_SEC}s)..."
echo ""

# gh run watch streams status updates efficiently via API
# It exits with the run's conclusion code
if gh run watch "$RUN_ID" --exit-status --timeout "$TIMEOUT_SEC" 2>&1; then
  echo ""
  echo "✓ CI PASSED for run $RUN_ID"
  exit 0
else
  exit_code=$?
  echo ""
  echo "✗ CI FAILED for run $RUN_ID (exit code $exit_code)"
  echo ""
  echo "--- Failed Jobs ---"
  gh run view "$RUN_ID" --json jobs --jq '.jobs[] | select(.conclusion=="failure") | "  - \(.name): \(.url)"' 2>/dev/null || true
  echo ""
  echo "--- Latest check job logs (errors) ---"
  gh run view "$RUN_ID" --log --job "check" 2>/dev/null | grep -E "(^error|^Error|error\[|Error\(|FAIL|failed|Compilation error|cannot find|not found|expected )" | head -30 || echo "(no common error patterns found)"
  echo ""
  echo "--- Full URL ---"
  gh run view "$RUN_ID" --json url --jq '.url' 2>/dev/null || echo "(url unavailable)"
  exit 1
fi
