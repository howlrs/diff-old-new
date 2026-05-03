"""Engle-Granger cointegration test の数値テスト."""

from __future__ import annotations

import numpy as np

from src.l2_features.spread import engle_granger_cointegration


def test_cointegrated_pair_detected() -> None:
    """合成: a = 0.5*b + ε で ε が定常 → cointegrated と判定される."""
    rng = np.random.default_rng(42)
    n = 500
    b = np.cumsum(rng.standard_normal(n))  # I(1)
    eps = rng.standard_normal(n) * 0.1  # I(0)
    a = 0.5 * b + 1.0 + eps  # cointegrated

    res = engle_granger_cointegration(a.tolist(), b.tolist())
    assert res.is_cointegrated, f"expected cointegrated, got pvalue={res.adf_pvalue}"
    assert abs(res.beta - 0.5) < 0.1
    assert res.half_life_bars is not None and res.half_life_bars > 0


def test_unrelated_random_walks_not_cointegrated() -> None:
    """二つの独立 RW は cointegrated と判定されない (高確率)."""
    rng = np.random.default_rng(7)
    n = 500
    a = np.cumsum(rng.standard_normal(n))
    b = np.cumsum(rng.standard_normal(n))
    res = engle_granger_cointegration(a.tolist(), b.tolist())
    # p value 0.05 を超える可能性が高い (false positive 5% 想定)
    assert res.adf_pvalue > 0.01, f"unexpectedly low pvalue {res.adf_pvalue}"


def test_short_series_returns_nan() -> None:
    res = engle_granger_cointegration([1.0, 2.0, 3.0], [1.0, 2.0, 3.0])
    assert not res.is_cointegrated
    assert res.n == 3
