"""distribution.py の Hill 推定 + Shapiro-Wilk + Welch t-test テスト."""

from __future__ import annotations

import numpy as np

from src.l2_features.distribution import (
    analyze_distribution,
    hill_estimator,
    shapiro_wilk_pvalue,
    welch_t_test,
)


def test_normal_data_not_heavy_tail() -> None:
    """正規分布: kurtosis ~0, shapiro p>0.05 で heavy=False."""
    rng = np.random.default_rng(42)
    samples = rng.standard_normal(500).tolist()
    res = analyze_distribution(samples)
    assert res.n == 500
    assert abs(res.kurtosis) < 1.0  # near 0
    assert res.shapiro_pvalue is not None
    assert not res.is_heavy_tail


def test_pareto_is_heavy_tail() -> None:
    """Pareto (alpha=2.5) の正値サンプル: heavy_tail=True."""
    rng = np.random.default_rng(7)
    samples = (rng.pareto(a=2.5, size=500) + 1).tolist()
    res = analyze_distribution(samples)
    assert res.is_heavy_tail


def test_hill_estimator_recovers_pareto_alpha() -> None:
    """Pareto(alpha=3) で hill_estimator が ~3 を返す (誤差許容)."""
    rng = np.random.default_rng(123)
    samples = (rng.pareto(a=3.0, size=2000) + 1).tolist()
    alpha = hill_estimator(samples, k_ratio=0.05)
    assert alpha is not None
    assert 1.5 < alpha < 6.0


def test_shapiro_short_series_returns_none() -> None:
    assert shapiro_wilk_pvalue([1.0, 2.0]) is None


def test_welch_t_test_separates_means() -> None:
    rng = np.random.default_rng(1)
    a = (rng.standard_normal(200) + 0.5).tolist()
    b = (rng.standard_normal(200) - 0.5).tolist()
    t, p = welch_t_test(a, b)
    assert p < 0.05
    assert t > 0  # a > b


def test_welch_t_test_same_distribution_no_significance() -> None:
    rng = np.random.default_rng(99)
    a = rng.standard_normal(200).tolist()
    b = rng.standard_normal(200).tolist()
    _, p = welch_t_test(a, b)
    assert p > 0.01  # likely > 0.01 with same distribution
