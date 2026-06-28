#!/usr/bin/env bash
set -euo pipefail

# Full test suite runner for ursnip-backend.
# Spins up a disposable Postgres container, runs all tests (including --ignored), then tears down.

COMPOSE_FILE="docker-compose.test.yml"
SERVICE_NAME="test-db"
DB_PORT=5433
DB_USER="ursnip_test"
DB_PASS="ursnip_test"
DB_NAME="ursnip_test"

export DATABASE_URL="postgres://${DB_USER}:${DB_PASS}@localhost:${DB_PORT}/${DB_NAME}"

cleanup() {
    echo ""
    echo "==> Tearing down test database..."
    docker compose -f "$COMPOSE_FILE" down -v --remove-orphans 2>/dev/null || true
}
trap cleanup EXIT

echo "==> Starting test database..."
docker compose -f "$COMPOSE_FILE" up -d --wait

echo "==> Running migrations..."
# sqlx-cli if available, otherwise the tests run migrations themselves
if command -v sqlx &>/dev/null; then
    sqlx migrate run --source ./migrations
else
    echo "    (sqlx-cli not found — tests will run migrations automatically)"
fi

echo ""
echo "==> Running unit tests + property tests (no DB required)..."
cargo test 2>&1 | tail -5

echo ""
echo "==> Running integration tests (DB required)..."
cargo test --tests -- --ignored --test-threads=1 2>&1

echo ""
echo "==> All tests complete!"
