#!/usr/bin/env bash
set -euo pipefail

PORT=4123
API_KEY="bench-key"
WIKI_ROOT=$(mktemp -d)
mkdir -p "$WIKI_ROOT/wiki" "$WIKI_ROOT/raw/assets"

export WIKI_ROOT PORT API_KEYS="bench:$API_KEY" PUBLIC_READ="true" LOG_LEVEL="warn" DB_PATH="$WIKI_ROOT/wikillm-api.db" HOST="127.0.0.1"

cleanup() {
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
  sleep 1
done

echo "Server ready. Warming up..."
sleep 2

BASE="http://127.0.0.1:$PORT"
AUTH="Authorization: Bearer $API_KEY"

# Seed realistic wiki: several pages with links, tags, sources, and an index
seed_wiki() {
  cat > "$WIKI_ROOT/wiki/overview.md" <<'EOF'
---
title: Overview
category: meta
tags: [overview, llm]
---

# Overview

This wiki tracks knowledge about LLM systems, entities, and summaries.
See [[concepts]] and the entity pages such as [[entity-25]].
EOF

  cat > "$WIKI_ROOT/wiki/concepts.md" <<'EOF'
---
title: Concepts
category: meta
tags: [concepts]
---

# Concepts

Key LLM concepts: retrieval, embeddings, reranking, knowledge graphs.
EOF

  mkdir -p "$WIKI_ROOT/wiki/entities"
  for i in $(seq 1 100); do
    cat > "$WIKI_ROOT/wiki/entities/entity-$i.md" <<EOF
---
title: Entity $i
category: entities
tags: [entity]
---

# Entity $i

Entity $i is a fictional company in the LLM ecosystem with product line $i.
Related: [[Overview]] and [[entity-$((i % 100 + 1))]].
EOF
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

RESULTS_FILE="/tmp/wikillm-bench-results.txt"
echo "WikiLLM API Benchmark Results" > "$RESULTS_FILE"
echo "================================" >> "$RESULTS_FILE"
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

# 1. Health endpoint scaling
run_bench "GET /health | concurrency=10" 10 "$BASE/health"
run_bench "GET /health | concurrency=50" 50 "$BASE/health"
run_bench "GET /health | concurrency=100" 100 "$BASE/health"
run_bench "GET /health | concurrency=200" 200 "$BASE/health"

# 2. Read page scaling
run_bench "GET /v1/pages/wiki/overview.md | concurrency=10" 10 "$BASE/v1/pages/wiki/overview.md"
run_bench "GET /v1/pages/wiki/overview.md | concurrency=50" 50 "$BASE/v1/pages/wiki/overview.md"
run_bench "GET /v1/pages/wiki/overview.md | concurrency=100" 100 "$BASE/v1/pages/wiki/overview.md"

run_bench "GET /v1/pages/wiki/entities/entity-25.md | concurrency=10" 10 "$BASE/v1/pages/wiki/entities/entity-25.md"
run_bench "GET /v1/pages/wiki/entities/entity-25.md | concurrency=50" 50 "$BASE/v1/pages/wiki/entities/entity-25.md"
run_bench "GET /v1/pages/wiki/entities/entity-25.md | concurrency=100" 100 "$BASE/v1/pages/wiki/entities/entity-25.md"

# 3. List pages
run_bench "GET /v1/pages?folder=wiki/entities&limit=50 | concurrency=10" 10 "$BASE/v1/pages?folder=wiki/entities&limit=50"
run_bench "GET /v1/pages?folder=wiki/entities&limit=50 | concurrency=50" 50 "$BASE/v1/pages?folder=wiki/entities&limit=50"

# 4. Full-text search (FTS mode; no embedder configured)
run_bench "GET /v1/search?q=LLM&limit=20 | concurrency=10" 10 "$BASE/v1/search?q=LLM&limit=20"
run_bench "GET /v1/search?q=Entity&limit=20 | concurrency=10" 10 "$BASE/v1/search?q=Entity&limit=20"

# 5. Update a popular page repeatedly (contended write)
run_bench "PUT /v1/pages/wiki/overview.md (contended update) | concurrency=1" 1 -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Overview\n\nUpdated content.","frontmatter":{"category":"meta","tags":["overview","updated"]}}' "$BASE/v1/pages/wiki/overview.md"
run_bench "PUT /v1/pages/wiki/overview.md (contended update) | concurrency=5" 5 -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Overview\n\nUpdated content.","frontmatter":{"category":"meta","tags":["overview","updated"]}}' "$BASE/v1/pages/wiki/overview.md"
run_bench "PUT /v1/pages/wiki/overview.md (contended update) | concurrency=10" 10 -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Overview\n\nUpdated content.","frontmatter":{"category":"meta","tags":["overview","updated"]}}' "$BASE/v1/pages/wiki/overview.md"

# 6. Create unique pages (autocannon <id> replacement: fresh path + body per request)
run_bench "PUT /v1/pages/wiki/notes/note-<id>.md (unique page creation) | concurrency=1" 1 --idReplacement -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Note <id>\n\nA journal-style note linking to [[Overview]].","frontmatter":{"category":"notes","tags":["daily","note"]}}' "$BASE/v1/pages/wiki/notes/note-<id>.md"
run_bench "PUT /v1/pages/wiki/notes/note-<id>.md (unique page creation) | concurrency=5" 5 --idReplacement -m PUT -H "$AUTH" -H "Content-Type: application/json" -b '{"content":"# Note <id>\n\nA journal-style note linking to [[Overview]].","frontmatter":{"category":"notes","tags":["daily","note"]}}' "$BASE/v1/pages/wiki/notes/note-<id>.md"

# 7. Upload raw sources (?force=true measures sustained overwrite, not one-shot 409s)
run_bench "POST /v1/sources/raw/uploads/file.md?force=true | concurrency=1" 1 -m POST -H "$AUTH" -H "Content-Type: text/plain" -b 'Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.' "$BASE/v1/sources/raw/uploads/file.md?force=true"
run_bench "POST /v1/sources/raw/uploads/file.md?force=true | concurrency=5" 5 -m POST -H "$AUTH" -H "Content-Type: text/plain" -b 'Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.' "$BASE/v1/sources/raw/uploads/file.md?force=true"

# 8. Append to log
run_bench "POST /v1/log/append | concurrency=1" 1 -m POST -H "$AUTH" -H "Content-Type: application/json" -b '{"message":"benchmark activity"}' "$BASE/v1/log/append"
run_bench "POST /v1/log/append | concurrency=5" 5 -m POST -H "$AUTH" -H "Content-Type: application/json" -b '{"message":"benchmark activity"}' "$BASE/v1/log/append"

# 9. Refresh index (rebuilds index.md)
run_bench "POST /v1/index/refresh | concurrency=1" 1 -m POST -H "$AUTH" "$BASE/v1/index/refresh"

# 10. Changes feed
run_bench "GET /v1/changes?limit=100 | concurrency=10" 10 "$BASE/v1/changes?limit=100"
run_bench "GET /v1/changes?path=wiki/overview.md&limit=20 | concurrency=10" 10 "$BASE/v1/changes?path=wiki/overview.md&limit=20"

# 11. Batch ingest (multi-file operation)
INGEST_BODY='{"source":{"title":"Article X","rel_path":"raw/article-x.md","content":"# Article X\n\nThis is a longer source document with multiple paragraphs of realistic text. Lorem ipsum dolor sit amet, consectetur adipiscing elit."},"operations":[{"rel_path":"wiki/summaries/Article X.md","content":"Summary of Article X with link to [[entity-x]].","frontmatter":{"category":"summaries"}},{"rel_path":"wiki/entities/x.md","content":"Entity X page referencing [[Overview]].","frontmatter":{"category":"entities"}}],"logEntry":"Article X"}'
run_bench "POST /v1/ingest | concurrency=1" 1 -m POST -H "$AUTH" -H "Content-Type: application/json" -b "$INGEST_BODY" "$BASE/v1/ingest"

# 12. Mixed read/write workloads are covered by scripts/benchmark-realistic.ts,
# which drives probabilistic client behaviors that a single autocannon
# request definition cannot express.

echo "" >> "$RESULTS_FILE"
echo "System info:" >> "$RESULTS_FILE"
echo "  OS:     $(uname -s -r -m)" >> "$RESULTS_FILE"
echo "  CPUs:   $(nproc)" >> "$RESULTS_FILE"
echo "  Memory: $(free -h | awk '/^Mem:/ {print $2}')" >> "$RESULTS_FILE"
echo "  Bun:    $(bun --version)" >> "$RESULTS_FILE"
echo "  Date:   $(date -Iseconds)" >> "$RESULTS_FILE"

cat "$RESULTS_FILE"
