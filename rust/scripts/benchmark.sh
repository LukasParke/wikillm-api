#!/usr/bin/env bash
# Benchmark the Rust WikiLLM API server.
set -euo pipefail

PORT=4124
API_KEY="bench-key"
WIKI_ROOT=$(mktemp -d)
DB_OUT=$(mktemp -d)
mkdir -p "$WIKI_ROOT/wiki" "$WIKI_ROOT/raw/assets"

export WIKI_ROOT PORT API_KEYS="bench:$API_KEY" PUBLIC_READ="true" LOG_LEVEL="error" \
       DB_BACKEND=sqlite DB_PATH="$DB_OUT/bench.db" HOST="127.0.0.1"

cleanup() {
  kill "$SERVER_PID" 2>/dev/null || true
  rm -rf "$WIKI_ROOT" "$DB_OUT"
}
trap cleanup EXIT

echo "Building release binary..."
cargo build --release 2>&1 | tail -1

echo "Starting Rust server on port $PORT..."
./target/release/wikillm-api > /tmp/rust-bench-server.log 2>&1 &
SERVER_PID=$!
for i in $(seq 1 30); do
  if curl -fsS "http://127.0.0.1:$PORT/health" > /dev/null 2>&1; then break; fi
  sleep 1
done
echo "Server ready. Warming up..."
sleep 2

BASE="http://127.0.0.1:$PORT"
AUTH="Authorization: Bearer $API_KEY"

# Seed wiki
seed_wiki() {
  cat > "$WIKI_ROOT/wiki/overview.md" <<'PAGE'
---
title: Overview
category: meta
tags: [overview, llm]
---

# Overview

This wiki tracks knowledge about LLM systems, entities, and summaries.
See [[concepts]] and the entity pages such as [[entity-25]].
PAGE
  cat > "$WIKI_ROOT/wiki/concepts.md" <<'PAGE'
---
title: Concepts
category: meta
tags: [concepts]
---

# Concepts

Key LLM concepts: retrieval, embeddings, reranking, knowledge graphs.
PAGE
  mkdir -p "$WIKI_ROOT/wiki/entities"
  for i in $(seq 1 100); do
    cat > "$WIKI_ROOT/wiki/entities/entity-$i.md" <<PAGE
---
title: Entity $i
category: entities
tags: [entity]
---

# Entity $i

Entity $i is a fictional company in the LLM ecosystem with product line $i.
Related: [[Overview]] and [[entity-$((i % 100 + 1))]].
PAGE
  done
  for i in $(seq 1 20); do
    printf 'Raw source document %i with lorem ipsum dolor sit amet.\n' "$i" > "$WIKI_ROOT/raw/source-$i.md"
  done
  cat > "$WIKI_ROOT/index.md" <<'EOF'
# Index

- [[Overview]] (wiki/overview.md)
- [[Concepts]] (wiki/concepts.md)
EOF
}
seed_wiki

RESULTS_FILE="/tmp/rust-bench-results.txt"
echo "WikiLLM API Rust Benchmark Results" > "$RESULTS_FILE"
echo "====================================" >> "$RESULTS_FILE"
echo "" >> "$RESULTS_FILE"

run_bench() {
  local name="$1"; shift
  local concurrency="$1"; shift
  echo "" >> "$RESULTS_FILE"
  echo "--- $name ---" >> "$RESULTS_FILE"
  echo "Running: $name"
  bunx autocannon -c "$concurrency" -d 10 "$@" 2>&1 | sed -n '/Stat.*2.5%/,$p' >> "$RESULTS_FILE" || true
  sleep 1
}

# 1. Health
run_bench "GET /health | c=10" 10 "$BASE/health"
run_bench "GET /health | c=50" 50 "$BASE/health"
run_bench "GET /health | c=100" 100 "$BASE/health"
run_bench "GET /health | c=200" 200 "$BASE/health"

# 2. Read pages
run_bench "GET /v1/pages/wiki/overview.md | c=10" 10 "$BASE/v1/pages/wiki/overview.md"
run_bench "GET /v1/pages/wiki/overview.md | c=50" 50 "$BASE/v1/pages/wiki/overview.md"
run_bench "GET /v1/pages/wiki/overview.md | c=100" 100 "$BASE/v1/pages/wiki/overview.md"
run_bench "GET /v1/pages/wiki/entities/entity-25.md | c=10" 10 "$BASE/v1/pages/wiki/entities/entity-25.md"
run_bench "GET /v1/pages/wiki/entities/entity-25.md | c=50" 50 "$BASE/v1/pages/wiki/entities/entity-25.md"
run_bench "GET /v1/pages/wiki/entities/entity-25.md | c=100" 100 "$BASE/v1/pages/wiki/entities/entity-25.md"

# 3. List pages
run_bench "GET /v1/pages?folder=wiki/entities&limit=50 | c=10" 10 "$BASE/v1/pages?folder=wiki/entities&limit=50"
run_bench "GET /v1/pages?folder=wiki/entities&limit=50 | c=50" 50 "$BASE/v1/pages?folder=wiki/entities&limit=50"

# 4. FTS search
run_bench "GET /v1/search?q=LLM&limit=20 | c=10" 10 "$BASE/v1/search?q=LLM&limit=20"
run_bench "GET /v1/search?q=Entity&limit=20 | c=10" 10 "$BASE/v1/search?q=Entity&limit=20"

# 5. Contended writes
run_bench "PUT /v1/pages/wiki/overview.md (contended) | c=1" 1 -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Overview\n\nUpdated.","frontmatter":{"category":"meta","tags":["overview"]}}' "$BASE/v1/pages/wiki/overview.md"
run_bench "PUT /v1/pages/wiki/overview.md (contended) | c=5" 5 -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Overview\n\nUpdated.","frontmatter":{"category":"meta","tags":["overview"]}}' "$BASE/v1/pages/wiki/overview.md"
run_bench "PUT /v1/pages/wiki/overview.md (contended) | c=10" 10 -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Overview\n\nUpdated.","frontmatter":{"category":"meta","tags":["overview"]}}' "$BASE/v1/pages/wiki/overview.md"

# 6. Unique page creation
run_bench "PUT unique pages | c=1" 1 --idReplacement -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Note <id>\n\nJournal note linking [[Overview]].","frontmatter":{"category":"notes","tags":["daily"]}}' "$BASE/v1/pages/wiki/notes/note-<id>.md"
run_bench "PUT unique pages | c=5" 5 --idReplacement -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Note <id>\n\nJournal note linking [[Overview]].","frontmatter":{"category":"notes","tags":["daily"]}}' "$BASE/v1/pages/wiki/notes/note-<id>.md"

# 7. Source uploads
run_bench "POST sources?force=true | c=1" 1 -m POST -H "$AUTH" -H "Content-Type: text/plain" -b 'Lorem ipsum dolor sit amet consectetur.' "$BASE/v1/sources/raw/uploads/f.md?force=true"
run_bench "POST sources?force=true | c=5" 5 -m POST -H "$AUTH" -H "Content-Type: text/plain" -b 'Lorem ipsum dolor sit amet consectetur.' "$BASE/v1/sources/raw/uploads/f.md?force=true"

# 8. Log append
run_bench "POST /v1/log/append | c=1" 1 -m POST -H "$AUTH" -H "Content-Type: application/json" -b '{"message":"benchmark"}' "$BASE/v1/log/append"
run_bench "POST /v1/log/append | c=5" 5 -m POST -H "$AUTH" -H "Content-Type: application/json" -b '{"message":"benchmark"}' "$BASE/v1/log/append"

# 9. Index refresh
run_bench "POST /v1/index/refresh | c=1" 1 -m POST -H "$AUTH" "$BASE/v1/index/refresh"

# 10. Changes feed
run_bench "GET /v1/changes?limit=100 | c=10" 10 "$BASE/v1/changes?limit=100"

# 11. Batch ingest
INGEST_BODY='{"source":{"title":"Article X","rel_path":"raw/article-x.md","content":"# Article X\n\nSource content."},"operations":[{"rel_path":"wiki/summaries/X.md","content":"Summary X."},{"rel_path":"wiki/entities/x.md","content":"Entity X."}],"log_entry":"X"}'
run_bench "POST /v1/ingest | c=1" 1 -m POST -H "$AUTH" -H "Content-Type: application/json" -b "$INGEST_BODY" "$BASE/v1/ingest"

echo "" >> "$RESULTS_FILE"
echo "System info:" >> "$RESULTS_FILE"
echo "  OS:     $(uname -s -r -m)" >> "$RESULTS_FILE"
echo "  CPUs:   $(nproc)" >> "$RESULTS_FILE"
echo "  Memory: $(free -h | awk '/^Mem:/ {print $2}')" >> "$RESULTS_FILE"
echo "  Runtime: Rust (release build)" >> "$RESULTS_FILE"
echo "  Backend: SQLite (FTS5)" >> "$RESULTS_FILE"
echo "  Date:   $(date -Iseconds)" >> "$RESULTS_FILE"

cat "$RESULTS_FILE"
