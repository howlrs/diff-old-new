# Phase 2 KPI: regime 間 IPD ドリフト有意差検定 (Welch's t-test)

v3 design §3 K1 の延長. R2 (週末) / R3 (CMEメンテ) / R4 (祝日) の IPD bar 分布が 統計的に異なるかを検定. p<0.05 なら戦略 H1 を regime 別にチューニング.

## xyz:SP500

| pair | n_a | n_b | t_stat | p_value | significant (p<0.05) |
|---|---|---|---|---|---|
| R2_closure_weekend_vs_R3_closure_daily | 6342 | 0 | - | - | (warn) |
| R2_closure_weekend_vs_R4_closure_holiday | 6342 | 0 | - | - | (warn) |
| R3_closure_daily_vs_R4_closure_holiday | 0 | 0 | - | - | (warn) |

## xyz:XYZ100

| pair | n_a | n_b | t_stat | p_value | significant (p<0.05) |
|---|---|---|---|---|---|
| R2_closure_weekend_vs_R3_closure_daily | 6342 | 0 | - | - | (warn) |
| R2_closure_weekend_vs_R4_closure_holiday | 6342 | 0 | - | - | (warn) |
| R3_closure_daily_vs_R4_closure_holiday | 0 | 0 | - | - | (warn) |

## BTC

| pair | n_a | n_b | t_stat | p_value | significant (p<0.05) |
|---|---|---|---|---|---|
| R2_closure_weekend_vs_R3_closure_daily | 6342 | 0 | - | - | (warn) |
| R2_closure_weekend_vs_R4_closure_holiday | 6342 | 0 | - | - | (warn) |
| R3_closure_daily_vs_R4_closure_holiday | 0 | 0 | - | - | (warn) |

## ETH

| pair | n_a | n_b | t_stat | p_value | significant (p<0.05) |
|---|---|---|---|---|---|
| R2_closure_weekend_vs_R3_closure_daily | 6342 | 0 | - | - | (warn) |
| R2_closure_weekend_vs_R4_closure_holiday | 6342 | 0 | - | - | (warn) |
| R3_closure_daily_vs_R4_closure_holiday | 0 | 0 | - | - | (warn) |

## 解釈ガイド

- **p < 0.05** = 2 regime の IPD 平均が有意に異なる
- 全 pair で有意 → regime 別パラメータ必須
- 全 pair で非有意 → 共通パラメータで H1 を統一できる