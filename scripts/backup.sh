#!/usr/bin/env bash
# Backup a WikiLLM API deployment: wiki folder + index store + connector state.
# The wiki folder is the source of truth; the store is derived cache, but a
# dump avoids a full re-embed on restore.
#
# Usage:
#   ./scripts/backup.sh /path/to/wiki /path/to/backup-dir [--pg URL]
#
# With --pg (or DATABASE_URL env) a plain pg_dump is written; otherwise the
# SQLite file next to the wiki (DB_PATH, default wikillm-api.db) is copied.
set -euo pipefail

WIKI="${1:?usage: backup.sh <wiki-root> <backup-dir> [--pg URL]}"
OUT="${2:?usage: backup.sh <wiki-root> <backup-dir> [--pg URL]}"
PG_URL="${4:-}"

mkdir -p "$OUT"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"

tar -czf "$OUT/wiki-$STAMP.tar.gz" -C "$(dirname "$WIKI")" "$(basename "$WIKI")"

if [[ "${3:-}" == "--pg" ]]; then
  PG_URL="$4"
fi

if [[ -n "$PG_URL" ]]; then
  docker run --rm -v "$OUT:/backup" postgres:16 \
    pg_dump --no-owner --format=custom -f "/backup/store-$STAMP.dump" "$PG_URL"
elif [[ -n "${DATABASE_URL:-}" ]]; then
  docker run --rm -v "$OUT:/backup" postgres:16 \
    pg_dump --no-owner --format=custom -f "/backup/store-$STAMP.dump" "$DATABASE_URL"
else
  DB_FILE="${DB_PATH:-$(pwd)/wikillm-api.db}"
  cp "$DB_FILE" "$OUT/store-$STAMP.db"
fi

echo "Backup complete:"
ls -la "$OUT" | grep "$STAMP"
