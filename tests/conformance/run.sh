#!/bin/sh
# Runs the official MCP conformance suite against **our** binary, on both revisions (core §12.2).
#
# The SDK passes this suite on its own; what this run proves is that *our handler* does not break
# it — by mis-classifying an error, returning malformed content, or losing deterministic
# ordering. It is also the alarm that fires when an SDK upgrade changes behaviour under us.
#
# Usage: tests/conformance/run.sh [--record]
#   --record  write the per-revision results under tests/conformance/results/ for inspection

set -e

CONFORMANCE_VERSION="0.2.0-alpha.11"
REVISIONS="2026-07-28 2025-11-25"
CONFIG_DIR="tests/conformance"
PORT=8177
URL="http://127.0.0.1:${PORT}/mcp"

if ! command -v npx > /dev/null 2>&1; then
    echo "conformance needs Node: 'npx' is not on PATH."
    echo "Install Node 20 or later, or run this target in the CI image that carries it."
    exit 1
fi

cargo build --quiet

LOG_LEVEL=warn ./target/debug/catalog-mcp-server --config-folder "${CONFIG_DIR}" > /tmp/catalog-mcp-conformance.log 2>&1 &
SERVER_PID=$!
# shellcheck disable=SC2064
trap "kill ${SERVER_PID} 2>/dev/null || true" EXIT INT TERM

# Wait for readiness rather than sleeping a guessed interval.
i=0
while [ $i -lt 50 ]; do
    if curl -sf "http://127.0.0.1:${PORT}/-/ready" > /dev/null 2>&1; then break; fi
    i=$((i + 1))
    sleep 0.2
done
if [ $i -ge 50 ]; then
    echo "the server did not become ready; its log follows:"
    cat /tmp/catalog-mcp-conformance.log
    exit 1
fi

STATUS=0
for revision in ${REVISIONS}; do
    echo ""
    echo "=== conformance: requirements ${revision} ==="

    EXTRA=""
    if [ "$1" = "--record" ]; then
        rm -rf "${CONFIG_DIR}/results/${revision}"
        mkdir -p "${CONFIG_DIR}/results/${revision}"
        EXTRA="-o ${CONFIG_DIR}/results/${revision}"
    fi

    # shellcheck disable=SC2086
    npx -y "@modelcontextprotocol/conformance@${CONFORMANCE_VERSION}" server \
        --url "${URL}" \
        --requirements "${revision}" \
        --expected-failures "${CONFIG_DIR}/expected-failures.${revision}.yaml" \
        ${EXTRA} || STATUS=1
done

exit ${STATUS}
