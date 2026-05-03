#!/usr/bin/env bash
# CI と同じコマンドをローカル実行して push 前に検証する.
# Gemini partner 推奨 (2026-05-04): CI 失敗を push 前に検出して再発防止.
#
# 使い方: bash scripts/check_ci_local.sh
# 推奨: git pre-push hook に登録 (.git/hooks/pre-push) or 手動実行

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

if [[ ! -d .venv ]]; then
    echo "ERROR: .venv not found. Run: python3.12 -m venv .venv && pip install -e '.[dev,all]'" >&2
    exit 1
fi

echo "=== CI replay ==="
echo ""

echo "[1/4] Install check (CI と同じ pip install)..."
.venv/bin/pip install -e ".[dev,all]" --quiet 2>&1 | tail -3

echo ""
echo "[2/4] Ruff lint..."
.venv/bin/ruff check src tests

echo ""
echo "[3/4] Ruff format check..."
.venv/bin/ruff format --check src tests

echo ""
echo "[4/4] Pytest (CI と同じ marker filter)..."
.venv/bin/pytest -q -m "not live and not slow" --cov=src --cov-report=term-missing | tail -5

echo ""
echo "=== ✓ All CI checks passed locally ==="
