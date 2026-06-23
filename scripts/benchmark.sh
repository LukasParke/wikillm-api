#!/usr/bin/env bash
set -euo pipefail

PORT=4123
API_KEY="bench-key"
WIKI_ROOT=$(mktemp -d)
mkdir -p "$WIKI_ROOT/wiki" "$WIKI_ROOT/raw/assets"

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

# Seed realistic wiki: several pages with links, tags, sources, and an index
seed_wiki() {
  echo "Seeding realistic wiki..."

  # Sources
  for i in $(seq 1 20); do
    curl -s -X POST "$BASE/v1/sources/raw/article-${i}.md" -H "$AUTH" -H "Content-Type: text/plain" \
      -d "# Source $i\n\nThis is the raw source text for article $i. It contains enough content to be realistic." > /dev/null
  done

  # Wiki pages with frontmatter, tags, categories, and inter-page links
  curl -s -X PUT "$BASE/v1/pages/wiki/index.md" -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"content":"# Index\n\n- [[Overview]]\n- [[Concepts]]\n- [[Entities]]","frontmatter":{"category":"meta"}}' > /dev/null

  curl -s -X PUT "$BASE/v1/pages/wiki/overview.md" -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"content":"# Overview\n\nHigh-level overview of the wiki. See [[Concepts]] and [[Entities]].","frontmatter":{"category":"meta","tags":["overview"]}}' > /dev/null

  curl -s -X PUT "$BASE/v1/pages/wiki/concepts.md" -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"content":"# Concepts\n\nCore concepts include LLMs, RAG, and wiki maintenance.","frontmatter":{"category":"concepts","tags":["llm","rag"]}}' > /dev/null

  for i in $(seq 1 50); do
    local tags=""
    if (( i % 2 == 0 )); then tags='"ai"'; else tags='"llm"'; fi
    curl -s -X PUT "$BASE/v1/pages/wiki/entities/entity-${i}.md" -H "$AUTH" -H "Content-Type: application/json" \
      -d "{\"content\":\"# Entity $i\\n\\nEntity $i is described here. Links to [[Concepts]], [[Overview]], and [[entity-$(( (i % 50) + 1 ))]].\",\"frontmatter\":{\"category\":\"entities\",\"tags\":[$tags],\"source\":\"article-${i}.md\"}}" > /dev/null
  done

  # Summary pages
  for i in $(seq 1 20); do
    curl -s -X PUT "$BASE/v1/pages/wiki/summaries/article-${i}-summary.md" -H "$AUTH" -H "Content-Type: application/json" \
      -d "{\"content\":\"# Summary of Article $i\\n\\nThis page summarizes [[raw/article-${i}.md|Article $i]] and links to [[entity-$i]].\",\"frontmatter\":{\"category\":\"summaries\",\"tags\":[\"summary\"]}}" > /dev/null
  done

  # Initial index refresh and log entry
  curl -s -X POST "$BASE/v1/index/refresh" -H "$AUTH" > /dev/null
  curl -s -X POST "$BASE/v1/log/append" -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"message":"seeded benchmark wiki","prefix":"bench"}' > /dev/null
}
seed_wiki

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

# 2. Read page scaling across different page types
run_bench "GET /v1/pages/wiki/overview.md | concurrency=10" 10 "$BASE/v1/pages/wiki/overview.md"
run_bench "GET /v1/pages/wiki/overview.md | concurrency=50" 50 "$BASE/v1/pages/wiki/overview.md"
run_bench "GET /v1/pages/wiki/overview.md | concurrency=100" 100 "$BASE/v1/pages/wiki/overview.md"

run_bench "GET /v1/pages/wiki/entities/entity-25.md | concurrency=10" 10 "$BASE/v1/pages/wiki/entities/entity-25.md"
run_bench "GET /v1/pages/wiki/entities/entity-25.md | concurrency=50" 50 "$BASE/v1/pages/wiki/entities/entity-25.md"
run_bench "GET /v1/pages/wiki/entities/entity-25.md | concurrency=100" 100 "$BASE/v1/pages/wiki/entities/entity-25.md"

# 3. List pages (scanning the cache)
run_bench "GET /v1/pages?folder=wiki/entities&limit=50 | concurrency=10" 10 "$BASE/v1/pages?folder=wiki/entities&limit=50"
run_bench "GET /v1/pages?folder=wiki/entities&limit=50 | concurrency=50" 50 "$BASE/v1/pages?folder=wiki/entities&limit=50"

# 4. Search across titles/bodies
run_bench "GET /v1/search?q=LLM&in=body&limit=20 | concurrency=10" 10 "$BASE/v1/search?q=LLM&in=body&limit=20"
run_bench "GET /v1/search?q=Entity&in=title&limit=20 | concurrency=10" 10 "$BASE/v1/search?q=Entity&in=title&limit=20"

# 5. Update a popular page repeatedly (contended write)
run_bench "PUT /v1/pages/wiki/overview.md (contended update) | concurrency=1" 1 -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Overview\n\nUpdated content.","frontmatter":{"category":"meta","tags":["overview","updated"]}}' "$BASE/v1/pages/wiki/overview.md"
run_bench "PUT /v1/pages/wiki/overview.md (contended update) | concurrency=5" 5 -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Overview\n\nUpdated content.","frontmatter":{"category":"meta","tags":["overview","updated"]}}' "$BASE/v1/pages/wiki/overview.md"
run_bench "PUT /v1/pages/wiki/overview.md (contended update) | concurrency=10" 10 -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Overview\n\nUpdated content.","frontmatter":{"category":"meta","tags":["overview","updated"]}}' "$BASE/v1/pages/wiki/overview.md"

# 6. Create unique pages across multiple directories
REQ_FILE=$(mktemp)
for i in $(seq 1 10000); do
  cat <<EOF
PUT $BASE/v1/pages/wiki/notes/note-${RANDOM}-${i}.md
Authorization: Bearer $API_KEY
Content-Type: application/json

{"content":"# Note $i\\n\\nA journal-style note with a link to [[Overview]] and tag #daily.","frontmatter":{"category":"notes","tags":["daily","note"]}}
EOF
done > "$REQ_FILE"
run_bench "PUT /v1/pages/wiki/notes/{unique}.md (with frontmatter/link) | concurrency=1" 1 -i "$REQ_FILE" "$BASE"
run_bench "PUT /v1/pages/wiki/notes/{unique}.md (with frontmatter/link) | concurrency=5" 5 -i "$REQ_FILE" "$BASE"
rm -f "$REQ_FILE"

# 7. Upload raw sources
run_bench "POST /v1/sources/raw/uploads/{unique}.md | concurrency=1" 1 -m POST -H "$AUTH" -H "Content-Type: text/plain" -b 'Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.' "$BASE/v1/sources/raw/uploads/file-0.md"
run_bench "POST /v1/sources/raw/uploads/{unique}.md | concurrency=5" 5 -m POST -H "$AUTH" -H "Content-Type: text/plain" -b 'Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.' "$BASE/v1/sources/raw/uploads/file-0.md"

# 8. Append to log
run_bench "POST /v1/log/append | concurrency=1" 1 -m POST -H "$AUTH" -H "Content-Type: application/json" -b '{"message":"benchmark activity"}' "$BASE/v1/log/append"
run_bench "POST /v1/log/append | concurrency=5" 5 -m POST -H "$AUTH" -H "Content-Type: application/json" -b '{"message":"benchmark activity"}' "$BASE/v1/log/append"

# 9. Refresh index (rebuilds index.md from cache)
run_bench "POST /v1/index/refresh | concurrency=1" 1 -m POST -H "$AUTH" "$BASE/v1/index/refresh"

# 10. Changes feed
run_bench "GET /v1/changes?limit=100 | concurrency=10" 10 "$BASE/v1/changes?limit=100"
run_bench "GET /v1/changes?path=wiki/overview.md&limit=20 | concurrency=10" 10 "$BASE/v1/changes?path=wiki/overview.md&limit=20"

# 11. Batch ingest (multi-file transaction)
INGEST_BODY='{"source":{"title":"Article X","rel_path":"raw/article-x.md","content":"# Article X\n\nThis is a longer source document with multiple paragraphs of realistic text. Lorem ipsum dolor sit amet, consectetur adipiscing elit."},"operations":[{"rel_path":"wiki/summaries/Article X.md","content":"Summary of Article X with link to [[entity-x]].","frontmatter":{"category":"summaries"}},{"rel_path":"wiki/entities/x.md","content":"Entity X page referencing [[Overview]].","frontmatter":{"category":"entities"}}],"logEntry":"Article X"}'
run_bench "POST /v1/ingest | concurrency=1" 1 -m POST -H "$AUTH" -H "Content-Type: application/json" -b "$INGEST_BODY" "$BASE/v1/ingest"

# 12. Mixed workload: 80% reads, 20% writes
MIXED_REQ_FILE=$(mktemp)
cat <<EOF > "$MIXED_REQ_FILE"
GET $BASE/health
GET $BASE/v1/pages/wiki/overview.md
GET $BASE/v1/pages/wiki/entities/entity-25.md
GET $BASE/v1/pages?folder=wiki/entities&limit=20
GET $BASE/v1/search?q=Entity&in=title&limit=10
PUT $BASE/v1/pages/wiki/mixed-write-target.md
Authorization: Bearer $API_KEY
Content-Type: application/json

{"content":"# Mixed write\n\nUpdated at {{$timestamp}}.","frontmatter":{"category":"mixed"}}
EOF
run_bench "Mixed workload (80% reads / 20% writes) | concurrency=10" 10 -i "$MIXED_REQ_FILE" "$BASE"
run_bench "Mixed workload (80% reads / 20% writes) | concurrency=50" 50 -i "$MIXED_REQ_FILE" "$BASE"
rm -f "$MIXED_REQ_FILE"

echo "" >> "$RESULTS_FILE"
echo "System info:" >> "$RESULTS_FILE"
echo "  OS:     $(uname -s -r -m)" >> "$RESULTS_FILE"
echo "  CPUs:   $(nproc)" >> "$RESULTS_FILE"
echo "  Memory: $(free -h | awk '/^Mem:/ {print $2}')" >> "$RESULTS_FILE"
echo "  Bun:    $(bun --version)" >> "$RESULTS_FILE"
echo "  Date:   $(date -Iseconds)" >> "$RESULTS_FILE"

cat "$RESULTS_FILE"
