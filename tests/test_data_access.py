"""GuiDataSource の安全な動作テスト (空ディレクトリで落ちない)."""

from __future__ import annotations

from pathlib import Path

from src.config import StorageConfig
from src.gui.data_access import GuiDataSource


def test_empty_dirs_do_not_crash(tmp_path: Path) -> None:
    storage = StorageConfig(
        raw_data_root=tmp_path / "raw",
        curated_data_root=tmp_path / "curated",
    )
    ds = GuiDataSource.from_config(storage, kpi_dir=tmp_path / "kpi")

    assert ds.list_symbols() == []
    assert ds.load_l2book().is_empty()
    assert ds.load_trades().is_empty()
    assert ds.load_asset_ctxs().is_empty()
    assert ds.load_features().is_empty()
    assert ds.list_backtest_strategies() == []
    assert ds.load_backtest_trades("nonexistent").is_empty()
    assert ds.load_kpi_json("nonexistent") == {}
    assert ds.load_kpi_md("nonexistent") == ""


def test_load_kpi_md_when_present(tmp_path: Path) -> None:
    storage = StorageConfig(
        raw_data_root=tmp_path / "raw",
        curated_data_root=tmp_path / "curated",
    )
    kpi = tmp_path / "kpi"
    kpi.mkdir()
    (kpi / "K1.md").write_text("# Hello", encoding="utf-8")
    ds = GuiDataSource.from_config(storage, kpi_dir=kpi)
    assert ds.load_kpi_md("K1") == "# Hello"
