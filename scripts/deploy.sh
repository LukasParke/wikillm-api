#!/usr/bin/env bash
set -euo pipefail

# WikiLLM API docker deployment helper.
# Reads environment from .env or current shell.

: "${WIKI_PATH:=./wiki}"
: "${PORT:=3000}"
: "${API_KEYS:?API_KEYS must be set}"
: "${IMAGE:=ghcr.io/lukasparke/wikillm-api}"
: "${TAG:=latest}"

export WIKI_PATH PORT API_KEYS IMAGE TAG

mkdir -p "$WIKI_PATH"

echo "Deploying WikiLLM API..."
echo "  Image:  ${IMAGE}:${TAG}"
echo "  Wiki:   $WIKI_PATH"
echo "  Port:   $PORT"

# Pull latest image unless building locally
if [ "${BUILD_LOCAL:-false}" = "true" ]; then
  docker compose -f docker-compose.yml up --build -d
else
  docker compose -f docker-compose.yml pull
  docker compose -f docker-compose.yml up -d
fi

echo "WikiLLM API is running. Health check:"
sleep 2
curl -fsS "http://localhost:${PORT}/health" || true
echo
