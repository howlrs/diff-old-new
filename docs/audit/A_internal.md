# Audit-A: internal consistency report

raw_root: `data/raw`
period: 2026-05-04 02:27:16.353000+09:00 → 2026-05-04 05:35:51.464000+09:00
symbols: 4

## 受信レイテンシ (recv_ts - exchange_ts, ms)

| symbol | n_l2 | median | p95 | p99 |
|---|---|---|---|---|
| BTC | 20034 | 343 | 558 | 785 |
| ETH | 20034 | 343 | 558 | 785 |
| xyz:SP500 | 20034 | 343 | 558 | 785 |
| xyz:XYZ100 | 20034 | 343 | 558 | 785 |

## 単調性 / 切断 / リカバリー

| symbol | dup | backward | long_gap_30s | recovery_snap |
|---|---|---|---|---|
| BTC | 0 | 0 | 1 | 0 |
| ETH | 0 | 0 | 1 | 0 |
| xyz:SP500 | 0 | 0 | 1 | 0 |
| xyz:XYZ100 | 0 | 0 | 1 | 0 |

## 価格ジャンプ (隣接バー間 mid 変化率)

| symbol | >1% | >5% |
|---|---|---|
| BTC | 0 | 0 |
| ETH | 0 | 0 |
| xyz:SP500 | 0 | 0 |
| xyz:XYZ100 | 0 | 0 |

## Oracle (asset_ctxs.oracle_px) vs Mid (l2book.mid)

| symbol | median diff (bps) | p95 abs diff (bps) |
|---|---|---|
| BTC | +4.6 | 6.9 |
| ETH | +4.5 | 7.9 |
| xyz:SP500 | -1.0 | 6.6 |
| xyz:XYZ100 | -2.2 | 6.7 |

## 板 健全性

| symbol | crossed bid>=ask | n=0 levels (%) |
|---|---|---|
| BTC | 0 | 0.00% |
| ETH | 0 | 0.00% |
| xyz:SP500 | 0 | 0.00% |
| xyz:XYZ100 | 0 | 0.00% |

## 解釈ガイド

- **latency**: WS 経由なら medianは数百 ms 以下が健全. p99 が秒オーダー超なら NTP / network 問題
- **dup / backward**: 0 が理想. backward > 0 はサーバー側 bug / clock skew
- **long_gap_30s**: 30 秒以上無受信の連続 → WS切断未検出 or recovery 漏れの疑い
- **mid jump >1%**: closure 中の oracle ワープを除き発生稀. 多発なら data corruption 疑い
- **oracle vs mid**: closure 中は乖離大きい (HL内部 EMA 由来) のが正常
- **板 crossed**: 0 が必須. >0 は重大 (受信順序逆転 / parsing bug)