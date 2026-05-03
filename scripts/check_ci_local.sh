#!/usr/bin/env bash
# CI と同じコマンドをローカル実行して push 前に検証する.
# Gemini partner 推奨 (2026-05-04): CI 失敗を push 前に検出して再発防止.
#
# 使い方: bash scripts/check_ci_local.sh
# 推奨: git pre-push hook に登録 (.git/hooks/pre-push) or 手動実行

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "=== CI replay ==="
echo ""

# ----- Python -----
if [[ -d .venv ]]; then
    echo "[python 1/4] Install check (CI と同じ pip install)..."
    .venv/bin/pip install -e ".[dev,all]" --quiet 2>&1 | tail -3

    echo ""
    echo "[python 2/4] Ruff lint..."
    .venv/bin/ruff check src tests

    echo ""
    echo "[python 3/4] Ruff format check..."
    .venv/bin/ruff format --check src tests

    echo ""
    echo "[python 4/4] Pytest (CI と同じ marker filter)..."
    .venv/bin/pytest -q -m "not live and not slow" --cov=src --cov-report=term-missing | tail -5
else
    echo "[python] .venv not found, skipping (run: python3.12 -m venv .venv && pip install -e '.[dev,all]')"
fi

# ----- Rust -----
if [[ -d executor && -f executor/Cargo.toml ]]; then
    echo ""
    echo "[rust 1/3] cargo fmt --check..."
    (cd executor && cargo fmt --all -- --check)

    echo ""
    echo "[rust 2/3] cargo clippy -D warnings..."
    (cd executor && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5)

    echo ""
    echo "[rust 3/3] cargo test..."
    (cd executor && cargo test --workspace 2>&1 | grep -E "test result|^error" | tail -10)
fi

echo ""
echo "=== ✓ All CI checks passed locally ==="
