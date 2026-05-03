# MARKET_MAKE アルゴリズム

> 実装: [`executor/crates/executor-algo/src/market_make.rs`](../../../executor/crates/executor-algo/src/market_make.rs)
> PR: [#63](https://github.com/howlrs/diff-old-new/pull/63)

## 役割

**target inventory 駆動の 2-sided ALO market making**。bid と ask を同時に出して
スプレッドを獲得しつつ, **目標ポジション** から逸脱したら quote サイズを skew (傾ける) して
inventory を target に近付ける。

ユーザー要望「**B 銘柄を market make で 100 long position make したい**」「**market make でポジション解消**」
が該当。target_size に最終的にしたい signed position を指定する。

## 動作フロー

```
loop:
  1. abort 信号 → 両 quote cancel → aborted で return
  2. started_instant.elapsed() >= max_total → 両 quote cancel → 最終位置で aborted/completed 判定
  3. drain_new_fills で fill 反映 + Progress::SliceFilled
     fill が起きた cloid を bid/ask Quote から落とす (次 iter で再 quote)
  4. AppState.position.size から current 取得 → delta = target - current
  5. |delta| <= target_tolerance_size なら両 quote cancel → break (完了)
  6. AppState.book snapshot → ensure_book_fresh → mid 取得
  7. quote_prices(mid, spread_bps_each_side) → (bid_px, ask_px)
  8. quote_sizes(delta, quote_size) → (bid_sz, ask_sz)  ※skew
  9. needs_repost(bid_quote, bid_px, bid_sz, repost_bps_threshold) なら:
       既存 bid cancel → 新 bid を ALO で enqueue
     ask 側も同様
  10. Progress::Heartbeat
  11. tokio::time::sleep(repost_poll)
```

## quote price の決定 (`quote_prices`)

```
factor   = spread_bps_each_side / 10000
bid_px   = mid * (1 - factor)
ask_px   = mid * (1 + factor)
```

例: mid=50000, spread_bps_each_side=10 → bid=49950, ask=50050。

## quote size の skew (`quote_sizes`)

```
delta = target - current  (positive = もっと long に振りたい)
cap   = quote_size * 2

delta == 0:        bid_sz = quote_size, ask_sz = quote_size  (中立)
delta > 0:         bid_sz = min(quote_size + delta, cap)
                   ask_sz = max(quote_size - delta, 0)
delta < 0:         bid_sz = max(quote_size - |delta|, 0)
                   ask_sz = min(quote_size + |delta|, cap)
```

| delta | bid_sz | ask_sz | 意味 |
|---|---|---|---|
| 0 | quote_size | quote_size | 中立 (両側等量) |
| +0.5 (quote_size=1) | 1.5 | 0.5 | bid 厚く, ask 薄く |
| +1.0 (quote_size=1) | 2.0 (cap) | 0.0 | bid のみ. ask 撤退 |
| +5.0 (quote_size=1) | 2.0 (cap) | 0.0 | 同上. cap で頭打ち |
| -0.7 (quote_size=1) | 0.3 | 1.7 | ask 厚く |

`ask_sz=0` のとき ask 側は post せず, BatchSender に何も送らない (cancel だけ)。

## repost 判定 (`needs_repost`)

```rust
match existing {
    None => new_sz > 0,                          // 初回 → 立てる (ただし sz>0 のときのみ)
    Some(_) if new_sz <= 0 => true,              // 撤退要求 → cancel
    Some(_) if old_sz != new_sz => true,         // size 変化 → repost
    Some(_) => moved_bps > repost_bps_threshold, // mid 移動が閾値超
}
```

## AlgoParams

| key | 型 | デフォルト | 説明 |
|---|---|---|---|
| `quote_size` | string→Decimal | **必須** | 各サイドのベースサイズ |
| `spread_bps_each_side` | string→Decimal | `"10"` | bid/ask 半スプレッド (bps) |
| `repost_bps_threshold` | string→Decimal | `"2"` | mid がこれだけ動いたら repost |
| `max_total_ms` | u32 | `300000` | 全体時間 (5 分) |
| `repost_poll_ms` | u32 | `250` | poll 周期 |
| `max_book_age_ms` | u32 | `500` | stale 検出 (0 で無効化) |
| `target_tolerance_size` | string→Decimal | `"0"` | `|target - current| ≤ tolerance` で完了 |

> **注意 (Gemini PR-6 review 反映)**:
> `repost_bps_threshold = 0` は "mid 変化のたび cancel/repost" となり HL レート制限を消費しがち。
> `tracing::warn!` ログで警告するが, error は返さない。本番では ≥1 推奨。

## 既知の制約 (PR-6 doc に明記)

1. **`all_fills` は無限拡張**: 長時間 MM 用途では ~1 時間ごとに execution をローテーション推奨,
   または disk 永続化するラッパーを用意 (将来課題)
2. **`BatchSender::enqueue` の Err は fatal**: channel closed = flusher 死 → 復旧不能なので
   `AlgoError::HyperliquidError` で abort

## 終了条件

| 条件 | 結果 |
|---|---|
| `|target - current| <= target_tolerance_size` | `aborted=false` |
| abort signal | `aborted=true` |
| `total_duration_ms` 経過 | `aborted = (|target-current| > tolerance)` |

## ユースケース

### A. 100 BTC 目標 (current 0 から開始)

```json
{
  "algorithm":   "market_make",
  "symbol":      "BTC",
  "intent":      "set_target",
  "target_size": "100.0",
  "params": {
    "quote_size":           "0.5",
    "spread_bps_each_side": "10",
    "repost_bps_threshold": "3",
    "max_total_ms":         86400000,
    "repost_poll_ms":       250
  }
}
```

quote_size=0.5 で常時 0.5 BTC 出しつつ, current=0 なら delta=100 (>>quote_size=0.5)
→ bid=1.0 (cap), ask=0.0 で **bid 1 本のみ** 出す状態が続く。
current が 100 に近付くにつれ ask が出始め, 100 で両側等量に。

### B. ポジション中立化 (target=0, current=+50)

```json
{
  "algorithm":   "market_make",
  "symbol":      "ETH",
  "intent":      "set_target",
  "target_size": "0",
  "params": {
    "quote_size": "0.5",
    "spread_bps_each_side": "8",
    "target_tolerance_size": "0.1",
    "max_total_ms": 7200000
  }
}
```

current=+50 → delta=-50 → bid=0, ask=1.0 (cap) で売り 1 本。0.1 までに収束したら終了。

### C. tight spread でリベート狙い

```json
{
  "params": {
    "quote_size":           "0.1",
    "spread_bps_each_side": "1",
    "repost_bps_threshold": "1"
  }
}
```

ALO は post-only なので cross したら拒否される (taker fill なし)。
ただし `repost_bps_threshold=1` だと頻繁に repost してレート制限を消費する点に注意。

## エラー

| エラー | 原因 |
|---|---|
| `quote_size is required` | params 必須 |
| `quote_size must be > 0` / `spread_bps_each_side must be >= 0` | validation |
| `market_make: empty book (no mid)` | book に bid/ask どちらかが無い |
| `book stale (...)` | WS 切断中 |
| `batch enqueue: ...` | flusher dead (fatal) |

## テスト (`market_make::tests`)

14 ケース:

- 5 件: quote price/size 計算 (中立, long skew, short skew, cap, neutral)
- 4 件: needs_repost (初回, size 変化, threshold 内, threshold 超)
- 1 件: from_algo_requires_quote_size
- 4 件: integration via MockHlClient
  - 中立 (delta=0) で両側 ALO at mid±10bps
  - 上方 skew (delta=+0.05, quote_size=0.1 → bid_sz=0.15, ask_sz=0.05)
  - target reach で exit
  - max_total timeout で cancel される

## 関連

- [PASSIVE_FOLLOW](passive_follow.md) — 1 サイドのみ
- [MARKET](market.md) / [TWAP](twap.md)
