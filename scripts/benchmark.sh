#!/usr/bin/env bash
set -euo pipefail

PORT=4123
API_KEY="bench-key"
WIKI_ROOT=$(mktemp -d)
mkdir -p "$WIKI_ROOT/wiki"

export WIKI_ROOT PORT API_KEYS="bench:$API_KEY" PUBLIC_READ="true" LOG_LEVEL="warn" DB_PATH="$WIKI_ROOT/wikillm-api.db" HOST="127.0.0.1"

cleanup() {
  echo "Cleaning up..."
  kill "$SERVER_PID" 2>/dev/null || true
  rm -rf "$WIKI_ROOT"
}
trap cleanup EXIT

echo "Starting server on port $PORT..."
bun run src/index.ts > /tmp/wikillm-bench-server.log 2>&1 &
SERVER_PID=$!

# Wait for server
for i in {1..30}; do
  if curl -fsS "http://127.0.0.1:$PORT/health" > /dev/null 2>&1; then
    break
  fi
  sleep 0.2
done

echo "Server ready. Warming up..."
sleep 2

BASE="http://127.0.0.1:$PORT"
AUTH="Authorization: Bearer $API_KEY"

# Seed a page for read tests
curl -s -X PUT "$BASE/v1/pages/wiki/bench.md" -H "$AUTH" -H "Content-Type: application/json" \
  -d '{"content":"# Bench\n\nTest page."}' > /dev/null

RESULTS_FILE="/tmp/wikillm-bench-results.txt"
echo "WikiLLM API Benchmark Results" > "$RESULTS_FILE"
echo "================================" >> "$RESULTS_FILE"
echo "" >> "$RESULTS_FILE"

run_bench() {
  local label="$1"
  shift
  echo "Running: $label"
  echo "--- $label ---" >> "$RESULTS_FILE"
  ./node_modules/.bin/autocannon --duration 10 --connections "$@" >> "$RESULTS_FILE" 2>&1
  echo "" >> "$RESULTS_FILE"
}

# 1. Health endpoint scaling
run_bench "GET /health | concurrency=10" 10 "$BASE/health"
run_bench "GET /health | concurrency=50" 50 "$BASE/health"
run_bench "GET /health | concurrency=100" 100 "$BASE/health"
run_bench "GET /health | concurrency=200" 200 "$BASE/health"

# 2. Read page scaling
run_bench "GET /v1/pages/wiki/bench.md | concurrency=10" 10 "$BASE/v1/pages/wiki/bench.md"
run_bench "GET /v1/pages/wiki/bench.md | concurrency=50" 50 "$BASE/v1/pages/wiki/bench.md"
run_bench "GET /v1/pages/wiki/bench.md | concurrency=100" 100 "$BASE/v1/pages/wiki/bench.md"

# 3. Update same page repeatedly
run_bench "PUT /v1/pages/wiki/bench.md (same page) | concurrency=1" 1 -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Updated"}' "$BASE/v1/pages/wiki/bench.md"
run_bench "PUT /v1/pages/wiki/bench.md (same page) | concurrency=5" 5 -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Updated"}' "$BASE/v1/pages/wiki/bench.md"
run_bench "PUT /v1/pages/wiki/bench.md (same page) | concurrency=10" 10 -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Updated"}' "$BASE/v1/pages/wiki/bench.md"

# 4. Batch ingest
# 4. Create unique pages
REQ_FILE=$(mktemp)
for i in $(seq 1 5000); do
  echo "PUT $BASE/v1/pages/wiki/page-${RANDOM}-${i}.md"
done > "$REQ_FILE"
run_bench "PUT /v1/pages/wiki/{unique}.md | concurrency=1" 1 -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Page"}' -i "$REQ_FILE" "$BASE"
rm -f "$REQ_FILE"

# 5. Batch ingest
INGEST_BODY='{"source":{"title":"Article X","rel_path":"raw/article-x.md","content":"# Article X"},"operations":[{"rel_path":"wiki/summaries/Article X.md","content":"Summary"},{"rel_path":"wiki/entities/X.md","content":"Entity X"}],"logEntry":"Article X"}'
run_bench "POST /v1/ingest | concurrency=1" 1 -m POST -H "$AUTH" -H "Content-Type: application/json" -b "$INGEST_BODY" "$BASE/v1/ingest"

echo "" >> "$RESULTS_FILE"
echo "System info:" >> "$RESULTS_FILE"
echo "  OS:     $(uname -s -r -m)" >> "$RESULTS_FILE"
echo "  CPUs:   $(nproc)" >> "$RESULTS_FILE"
echo "  Memory: $(free -h | awk '/^Mem:/ {print $2}')" >> "$RESULTS_FILE"
echo "  Bun:    $(bun --version)" >> "$RESULTS_FILE"
echo "  Date:   $(date -Iseconds)" >> "$RESULTS_FILE"

cat "$RESULTS_FILE"
