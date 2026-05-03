# Phase 2 KPI: 分布のファットテール定量化

v3 design §3 K10/K11 の前段. regime 別 IPD 分布の正規性を Hill 推定 + Shapiro-Wilk で検定.

## xyz:SP500

| regime | n | mean | std | skew | kurt | hill_alpha | shapiro_p | heavy? |
|---|---|---|---|---|---|---|---|---|
| R1_active | 0 | - | - | - | - | - | - | - |
| R2_closure_weekend | 6342 | -0.048 | 3.221 | +0.75 | -0.33 | 3.52 | 0.0000 | YES |
| R3_closure_daily | 0 | - | - | - | - | - | - | - |
| R4_closure_holiday | 0 | - | - | - | - | - | - | - |

## xyz:XYZ100

| regime | n | mean | std | skew | kurt | hill_alpha | shapiro_p | heavy? |
|---|---|---|---|---|---|---|---|---|
| R1_active | 0 | - | - | - | - | - | - | - |
| R2_closure_weekend | 6342 | +3.894 | 12.951 | +0.30 | -0.21 | 3.66 | 0.0000 | YES |
| R3_closure_daily | 0 | - | - | - | - | - | - | - |
| R4_closure_holiday | 0 | - | - | - | - | - | - | - |

## BTC

| regime | n | mean | std | skew | kurt | hill_alpha | shapiro_p | heavy? |
|---|---|---|---|---|---|---|---|---|
| R1_active | 0 | - | - | - | - | - | - | - |
| R2_closure_weekend | 6342 | -35.018 | 7.283 | -0.42 | -0.26 | 13.00 | 0.0000 | no |
| R3_closure_daily | 0 | - | - | - | - | - | - | - |
| R4_closure_holiday | 0 | - | - | - | - | - | - | - |

## ETH

| regime | n | mean | std | skew | kurt | hill_alpha | shapiro_p | heavy? |
|---|---|---|---|---|---|---|---|---|
| R1_active | 0 | - | - | - | - | - | - | - |
| R2_closure_weekend | 6342 | -1.018 | 0.156 | +0.39 | +0.77 | 14.14 | 0.0000 | no |
| R3_closure_daily | 0 | - | - | - | - | - | - | - |
| R4_closure_holiday | 0 | - | - | - | - | - | - | - |

## 解釈ガイド

- **kurt > 3**: 正規分布よりピーク鋭く, 裾が厚い (heavy-tail)
- **hill_alpha < 4**: tail がべき乗則に近い heavy-tail
- **shapiro_pvalue < 0.05**: 正規性棄却 (CLT 収束遅い)
- heavy=YES の regime は LLN/CLT で平均±k·σ/√N の信頼区間が信用できない
- Phase 2 採否判定: heavy-tail 銘柄/regime は分布 tail を直接狙う戦略にする