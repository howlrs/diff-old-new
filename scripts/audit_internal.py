"""Audit-A: 内部整合性監査 entry point."""

from __future__ import annotations

import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))

from src.audit.internal_consistency import render_markdown, run_audit  # noqa: E402
from src.config import load_config  # noqa: E402


def main() -> None:
    cfg = load_config(
        [
            REPO_ROOT / "config" / "default.yaml",
            REPO_ROOT / "config" / "local.yaml",
        ]
    )
    report = run_audit(cfg.storage.raw_data_root)
    md = render_markdown(report)
    out_dir = REPO_ROOT / "docs" / "audit"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / "A_internal.md"
    out_path.write_text(md, encoding="utf-8")
    print(md)
    print(f"\n[saved] {out_path}")


if __name__ == "__main__":
    main()
