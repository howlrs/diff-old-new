"""Audit-A0: schema sanity check (前提検査)."""

from __future__ import annotations

import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))

from src.audit.schema_check import check_schema, render_markdown  # noqa: E402
from src.config import load_config  # noqa: E402


def main() -> None:
    cfg = load_config(
        [
            REPO_ROOT / "config" / "default.yaml",
            REPO_ROOT / "config" / "local.yaml",
        ]
    )
    result = check_schema(cfg.storage.raw_data_root)
    md = render_markdown(result)
    out_dir = REPO_ROOT / "docs" / "audit"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / "A0_schema.md"
    out_path.write_text(md, encoding="utf-8")
    print(md)
    print(f"\n[saved] {out_path}")
    sys.exit(0 if result.all_healthy else 1)


if __name__ == "__main__":
    main()
