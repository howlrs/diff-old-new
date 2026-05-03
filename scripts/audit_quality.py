"""Audit-D: data quality score 計算 entry point. A/B 結果を集約."""

from __future__ import annotations

import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))

from src.audit.external_benchmark import run_external_audit  # noqa: E402
from src.audit.internal_consistency import run_audit  # noqa: E402
from src.audit.quality_score import compute_quality_score, render_markdown  # noqa: E402
from src.config import load_config  # noqa: E402


def main() -> None:
    cfg = load_config(
        [
            REPO_ROOT / "config" / "default.yaml",
            REPO_ROOT / "config" / "local.yaml",
        ]
    )
    print("[audit] running internal consistency...")
    internal = run_audit(cfg.storage.raw_data_root)
    print("[audit] running external benchmark...")
    try:
        external = run_external_audit(cfg.storage.raw_data_root)
    except Exception as exc:
        print(f"[audit] external skipped: {exc}")
        external = None

    cards = compute_quality_score(internal, external)
    md = render_markdown(cards)
    out_dir = REPO_ROOT / "docs" / "audit"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / "D_quality_score.md"
    out_path.write_text(md, encoding="utf-8")
    print(md)
    print(f"\n[saved] {out_path}")

    # exit code: critical (<80) は 1
    if any(card.score < 80 for card in cards.values()):
        sys.exit(1)


if __name__ == "__main__":
    main()
