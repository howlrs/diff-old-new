# TWAP アルゴリズム

> 実装: [`executor/crates/executor-algo/src/twap.rs`](../../../executor/crates/executor-algo/src/twap.rs)
> PR: [#62](https://github.com/howlrs/diff-old-new/pull/62)

## 役割

**Time-Weighted Average Price**。`abs_size` を `slice_count` 等分し, `total_duration_ms / slice_count`
ごとに 1 slice ずつ執行する。child algorithm として `market` (taker IOC) または `passive` (maker ALO) を選べる。

ユーザー要望「**現在の A ポジションを TWAP で解除してください**」がそのまま該当。

## 動作フロー

```
1. ctx.params から TwapParams 抽出 (slice_count > 0, total_duration > 0 等)
2. AppState.position から現在ポジション snapshot
3. resolve_side_and_size() で side + abs_size 算出
4. slice_size_base = abs_size / slice_count
   interval       = total_duration / slice_count
5. for slice_idx in 1..=slice_count:
   a. abort/timeout チェック
   b. target_at_slice = slice_size_base * slice_idx
      (最終 slice は abs_size 丸めを吸収するため target_at_slice = abs_size)
   c. slice_target_remaining = max(0, target_at_slice - filled_so_far)
      (= 前 slice が under-fill していたら今 slice で取り戻す)
   d. AppState.book snapshot → ensure_book_fresh
   e. 旧 passive quote があれば cancel
   f. child = Market: taker IOC を slice_target_remaining サイズで enqueue
      child = Passive: ALO を touch に slice_target_remaining サイズで enqueue
   g. wait = (Market: min(slice_timeout, interval)) / (Passive: interval)
      で fill を drain しつつ poll
   h. Progress::Heartbeat 送出
6. 残 passive quote cancel
7. 最終 drain_new_fills (straggler 救済)
8. build_report (filled < abs_size なら aborted=true)
```

### 累積目標方式 (重要設計)

`target_at_slice = slice_size_base * slice_idx` を使うため,
slice N で under-fill しても slice N+1 で **累積目標まで取りに行く**。
これで「先に fail した slice の量を捨てない」挙動になる。

最終 slice (`slice_idx == slice_count`) は浮動少数の累積誤差を吸収するため
`target_at_slice = abs_size` に固定される。

## AlgoParams

| key | 型 | デフォルト | 説明 |
|---|---|---|---|
| `slice_count` | u32 | `10` | スライス数。0 はエラー |
| `total_duration_ms` | u32 | `60000` | 全体時間。0 はエラー |
| `child_algo` | string | `"market"` | `"market"` / `"passive"` (`"passive_follow"` も可) |
| `max_slippage_bps` | string→Decimal | `"20"` | child=market 時の slippage cap |
| `slice_timeout_ms` | u32 | `1500` | child=market の 1 slice fill 待機 (interval を超える場合は interval 採用) |
| `max_book_age_ms` | u32 | `500` | book stale 検出。`0` で無効化 (test 用途) |
| `reduce_only` | bool | `false` | reduce_only flag |

## child=market vs child=passive の違い

| 観点 | child=market | child=passive |
|---|---|---|
| TIF | IOC | ALO (post-only) |
| 価格 | best_ask (long) ± slippage | best_bid (long) / best_ask (short) — touch 同価 |
| fill 期待値 | 高い (taker) | 低い (maker, スキップ slice もあり) |
| 手数料 | テイク | リベート (HL のフィーモデル次第) |
| under-fill 時 | 次 slice で累積取り直し | 同じく累積方式 |

**MARKET_MAKE と passive child の違い**:
- TWAP/passive: **片側 1 本のみ**, slice 区切りで再計算
- MARKET_MAKE: **両側 2 本**, repost loop が常時稼働 (target は不変, drift で skew)

## 終了条件

| 条件 | 結果 |
|---|---|
| 全 slice 完走 + filled >= abs_size | `aborted=false`, completed |
| 全 slice 完走 + filled < abs_size | `aborted=true`, `abort_reason="twap finished with X of Y filled"` |
| abort signal | `aborted=true`, `abort_reason="aborted by caller"` |
| `total_duration` 経過 | `aborted=true`, `abort_reason="total_duration (...) elapsed at slice N"` |

## ユースケース

### A. 1 時間 TWAP で 100 BTC 解除 (taker)

```json
{
  "algorithm":   "twap",
  "symbol":      "BTC",
  "intent":      "close",
  "target_size": "0",
  "params": {
    "slice_count":       60,
    "total_duration_ms": 3600000,
    "child_algo":        "market",
    "max_slippage_bps":  "30"
  }
}
```

1 分毎に IOC を投入。

### B. 10 分 TWAP で 5 BTC 取得 (maker, リベート狙い)

```json
{
  "algorithm":   "twap",
  "symbol":      "BTC",
  "intent":      "open",
  "target_size": "5.0",
  "params": {
    "slice_count":       10,
    "total_duration_ms": 600000,
    "child_algo":        "passive"
  }
}
```

1 分毎に ALO を 0.5 BTC ずつ. fill 不足は次 slice で取り戻し。

## エラー

| エラー | 原因 |
|---|---|
| `slice_count must be > 0` | 仕様違反 |
| `total_duration_ms must be > 0` | 仕様違反 |
| `child_algo must be one of ...` | 不明な child name |
| `max_slippage_bps must be >= 0` | 負の slippage |
| `twap: empty asks` / `twap: empty bids` | book に touch なし |
| `book stale (...)` | WS 切断中 |

## テスト (`twap::tests`)

9 ケース:

- 4 件: params validation (`parse_child_algo`, slice_count=0, total_duration=0, 負 slippage)
- 5 件: integration via MockHlClient
  - 5 slice market で full fill (5 distinct cloids)
  - 3 slice passive (ALO at best_bid)
  - close-short via TWAP (side=Long, sz=0.25 per slice)
  - 即時 abort
  - total_duration timeout

## 関連

- [MARKET](market.md) — child として呼ばれる taker
- [PASSIVE_FOLLOW](passive_follow.md) — child=passive の概念ベース
- [MARKET_MAKE](market_make.md) — 両側 maker
