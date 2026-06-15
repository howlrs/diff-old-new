# Release Notes — v0.5.1

Date: 2026-06-15

## Summary

Hyperliquid official free API だけで、対象銘柄に対する不特定多数の TWAP 実行量をサンプル推定する監視スクリプトを追加した。

## Added

- `scripts/hl_public_twap_monitor.py`
  - `recentTrades` から対象銘柄の直近アクティブユーザーを発見。
  - `userTwapSliceFills` から selected users の TWAP slice fills を集計。
  - `userFills` から selected users の対象銘柄全約定量を集計。
  - 直近 1 時間 window と 12 時間前の 1 時間 window を比較。
  - `--sample-rank-by` で `target_volume` / `target_fills` / `all_volume` / `all_fills` / `account_value` / `input_order` を選択可能。
  - `--sample-report-mode` で `twap-notional` / `twap-share` / `all` を選択可能。

## Report modes

| mode | 内容 |
|---|---|
| `twap-notional` | 既存の TWAP 総額 current/past 比較 |
| `twap-share` | TWAP 総額に加えて、sampled total volume に対する TWAP share を出力 |
| `all` | `twap-share` に加えて BUY/SELL TWAP notional imbalance を出力 |

## Docs

- [`operations/hyperliquid-public-twap-monitor.md`](operations/hyperliquid-public-twap-monitor.md)
- [`HANDOFF-2026-06-15-v0.5.1.md`](HANDOFF-2026-06-15-v0.5.1.md)

## Verification

```bash
.venv/bin/python -m py_compile scripts/hl_public_twap_monitor.py
.venv/bin/ruff check scripts/hl_public_twap_monitor.py
```

公式 free API で `twap-notional` / `twap-share` / `all` の小サンプル実行を確認済み。

## Known limitations

- 全銘柄・全ユーザーを完全 scan するものではなく、`recentTrades` から発見した候補ユーザーのサンプル推定。
- past window も current で selected された同一ユーザー群を対象に比較する。
- SELL TWAP はヘッジ、ポジション解消、ショート構築が混在するため、方向シグナルとしては補助指標扱い。

