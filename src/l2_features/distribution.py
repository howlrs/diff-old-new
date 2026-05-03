"""分布解析: Hill 推定 + Shapiro-Wilk + 正規性 Q-Q 評価.

LLN/CLT 適用可否の判断に使う. heavy-tail (Hill alpha < 4) なら CLT 収束は遅く,
小サンプルで信頼区間を作っても overfitting する.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
from scipy import stats


@dataclass
class DistributionStats:
    n: int
    mean: float
    std: float
    skewness: float
    kurtosis: float  # Fisher 定義 (正規分布で 0)
    hill_alpha: float | None  # tail index, < 4 で heavy-tail
    shapiro_pvalue: float | None  # 正規性 (>0.05 で正規性棄却できず)
    is_heavy_tail: bool


def hill_estimator(values: list[float] | np.ndarray, k_ratio: float = 0.1) -> float | None:
    """Hill 推定で tail index alpha を求める.

    alpha が小さいほどヘビーテール. 一般に alpha < 4 で fat-tail とみなす.
    上位 k = n * k_ratio 件の order statistics を使う.

    Returns:
        alpha: 推定 tail index. データ少なすぎ・分散ゼロ等は None.
    """
    arr = np.asarray(values, dtype=float)
    arr = arr[np.isfinite(arr) & (arr > 0)]  # 正値のみ (右側 tail)
    n = len(arr)
    if n < 30:
        return None
    arr_sorted = np.sort(arr)[::-1]  # 降順
    k = max(int(n * k_ratio), 10)
    if k >= n:
        k = n - 1
    top_k = arr_sorted[:k]
    threshold = arr_sorted[k]
    if threshold <= 0:
        return None
    log_ratios = np.log(top_k / threshold)
    if log_ratios.sum() <= 0:
        return None
    alpha = 1.0 / np.mean(log_ratios)
    return float(alpha)


def shapiro_wilk_pvalue(values: list[float] | np.ndarray) -> float | None:
    """Shapiro-Wilk p-value (>0.05 = 正規性を棄却できない).

    n>5000 では使えないので scipy.stats.normaltest にフォールバック.
    """
    arr = np.asarray(values, dtype=float)
    arr = arr[np.isfinite(arr)]
    n = len(arr)
    if n < 8:
        return None
    if n > 5000:
        # large sample: D'Agostino-Pearson normaltest
        _, p = stats.normaltest(arr)
        return float(p)
    _, p = stats.shapiro(arr)
    return float(p)


def analyze_distribution(values: list[float] | np.ndarray) -> DistributionStats:
    """分布の総合解析."""
    arr = np.asarray(values, dtype=float)
    arr = arr[np.isfinite(arr)]
    n = len(arr)
    if n == 0:
        return DistributionStats(0, 0.0, 0.0, 0.0, 0.0, None, None, False)
    mean = float(arr.mean())
    std = float(arr.std())
    skew = float(stats.skew(arr)) if n > 2 else 0.0
    kurt = float(stats.kurtosis(arr)) if n > 3 else 0.0  # Fisher (正規 = 0)
    abs_arr = np.abs(arr)
    abs_arr = abs_arr[abs_arr > 0]
    hill_alpha = hill_estimator(abs_arr) if len(abs_arr) >= 30 else None
    shapiro_p = shapiro_wilk_pvalue(arr)
    is_heavy = (hill_alpha is not None and hill_alpha < 4.0) or (kurt > 3.0)
    return DistributionStats(
        n=n,
        mean=mean,
        std=std,
        skewness=skew,
        kurtosis=kurt,
        hill_alpha=hill_alpha,
        shapiro_pvalue=shapiro_p,
        is_heavy_tail=is_heavy,
    )


def welch_t_test(
    sample_a: list[float] | np.ndarray,
    sample_b: list[float] | np.ndarray,
) -> tuple[float, float]:
    """2 サンプルの Welch t-test (不等分散).

    Returns:
        (t_statistic, p_value).  p < 0.05 で有意差あり.
    """
    a = np.asarray(sample_a, dtype=float)
    b = np.asarray(sample_b, dtype=float)
    a = a[np.isfinite(a)]
    b = b[np.isfinite(b)]
    if len(a) < 2 or len(b) < 2:
        return (float("nan"), 1.0)
    t, p = stats.ttest_ind(a, b, equal_var=False)
    return (float(t), float(p))
