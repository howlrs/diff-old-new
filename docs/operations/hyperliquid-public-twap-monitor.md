# Hyperliquid public TWAP monitor

最終更新: 2026-06-15

`scripts/hl_public_twap_monitor.py` は、対象銘柄に対して不特定多数の TWAP 約定量を推定するための観測スクリプト。

## 目的

対象銘柄について、直近 1 時間 window と 12 時間前の 1 時間 window を比較し、継続的なポジション構築・解消の強度を見る。

主な比較軸:

- TWAP 総額: `userTwapSliceFills` 由来の TWAP notional を BUY / SELL 別に current vs past で比較する。
- TWAP share: 同じ監視対象ウォレットの対象銘柄全約定 notional に対し、TWAP notional が何割かを見る。
- TWAP imbalance: `(BUY_TWAP_NOTIONAL - SELL_TWAP_NOTIONAL) / (BUY_TWAP_NOTIONAL + SELL_TWAP_NOTIONAL)` で BUY / SELL の偏りを見る。

## 情報源

無料で取得できる Hyperliquid official info endpoint を使う。

- `recentTrades`: 対象銘柄の直近取引から候補ユーザーを発見する。
- `userFills`: 候補ユーザーの対象銘柄全約定量を集計し、ランキングや TWAP share の分母に使う。
- `userTwapSliceFills`: 候補ユーザーの TWAP slice fill を集計し、TWAP notional の分子に使う。

Nansen などの加工済み外部データは使わない。QuickNode などの HyperCore node provider も必須ではない。

## 基本コマンド

既存の TWAP 総額比較だけを見る。

```bash
.venv/bin/python scripts/hl_public_twap_monitor.py \
  --coin HYPE \
  --user-sample \
  --discover-users recent-trades \
  --sample-rank-by target_volume \
  --sample-top-n 50 \
  --sample-concurrency 1 \
  --sample-report-mode twap-notional
```

TWAP share も見る。

```bash
.venv/bin/python scripts/hl_public_twap_monitor.py \
  --coin HYPE \
  --user-sample \
  --discover-users recent-trades \
  --sample-rank-by target_volume \
  --sample-top-n 50 \
  --sample-concurrency 1 \
  --sample-report-mode twap-share
```

TWAP share と BUY/SELL imbalance まで見る。

```bash
.venv/bin/python scripts/hl_public_twap_monitor.py \
  --coin HYPE \
  --user-sample \
  --discover-users recent-trades \
  --sample-rank-by target_volume \
  --sample-top-n 50 \
  --sample-concurrency 1 \
  --sample-report-mode all
```

## ユーザーランキング

`--sample-rank-by` で候補ユーザーの選び方を切り替える。

| 値 | 意味 |
|---|---|
| `target_volume` | 対象銘柄の直近出来高上位 |
| `target_fills` | 対象銘柄の直近約定回数上位 |
| `all_volume` | ユーザー全体の約定notional上位 |
| `all_fills` | ユーザー全体の約定回数上位 |
| `account_value` | account value 上位 |
| `input_order` | 入力順 |

公式 free API の `recentTrades` discovery だけで完結する軽量運用では、まず `target_volume` か `target_fills` を使う。

## window

デフォルトは以下。

- current: 直近 1 時間
- past: 12 時間前を終点にした 1 時間

変更する場合:

```bash
--window-s 3600 --compare-offset-s 43200
```

## 出力の読み方

`ratio` は current / past。

- `notional_ratio_current_over_past`: TWAP notional の current / past。
- `twap_notional_share_current`: current window で、監視対象ウォレットの対象銘柄全約定 notional に占める TWAP notional。
- `twap_notional_share_ratio_current_over_past`: TWAP share の current / past。
- `TWAP notional imbalance`: BUY 側に寄ると正、SELL 側に寄ると負。

SELL 側は HYPE ヘッジ、ポジション解消、ショート構築が混ざるため、単独では方向シグナルとして扱いすぎない。

## 限界

- 公式 API は全ユーザー全履歴の銘柄別 TWAP scan を返さないため、`recentTrades` から発見した候補ユーザーのサンプル推定になる。
- current と past の監視対象ウォレットは、同じ selected users を使う。つまり「現在の出来高上位を選び、そのウォレット群が 12 時間前 window でどうだったか」を見る。
- `recentTrades` の観測 window が短いと、サンプルが偏る。必要なら `--discover-seconds` と `--discover-max-trades` を増やす。
- top_n を大きくすると `userFills` / `userTwapSliceFills` の呼び出し数が増える。429 回避のため `--sample-concurrency 1` から始める。

