# PASSIVE_FOLLOW アルゴリズム

> 実装: [`executor/crates/executor-algo/src/passive_follow.rs`](../../../executor/crates/executor-algo/src/passive_follow.rs)
> PR: [#61](https://github.com/howlrs/diff-old-new/pull/61)

## 役割

**maker (メイカー) スタイルの執行**。常に **best bid** (long) または **best ask** (short) に
ALO (Add-Liquidity-Only / post-only) 指値を 1 本だけ resting させ続け, 板の touch が動いたら
個別 cancel + 即時 repost で追従する。

ユーザー要望「market make で 100 long position を作りたい」「market make でポジション解消したい」
の "market make 風 1 サイド執行" に対応 (両建て maker は [MARKET_MAKE](market_make.md) 側)。

## 動作フロー

```
1. ctx.params から PassiveFollowParams 抽出
2. AppState.position から現在ポジション snapshot
3. resolve_side_and_size() で side + abs_size 算出 (MARKET と同じ規則)
4. loop:
   a. abort 信号 → 残 quote cancel → aborted で return
   b. started_instant.elapsed() >= max_total → 残 quote cancel → aborted で return
   c. drain_new_fills() で fill 反映 → all_fills/remaining 更新, Progress::SliceFilled 送出
   d. remaining <= 0 なら break (完了)
   e. AppState.book snapshot → ensure_book_fresh()
   f. touch_for_side() で best_bid (long) / best_ask (short)
   g. 既存 quote と price 差ありなら:
       - 旧 cloid を CancelIntent で BatchSender enqueue
       - 新 cloid で ALO OrderIntent を BatchSender enqueue (size = remaining)
   h. tokio::time::sleep(repost_poll)
5. build_report() → Progress::Completed
```

**1 タイミングに resting 注文は最大 1 本** (両側持たない)。両建て市場メイクは MARKET_MAKE 側の役割。

## maker 側の touch 選択 (`touch_for_side`)

```
side = Long  (買いたい): touch = best_bid → 列に並ぶ
side = Short (売りたい): touch = best_ask → 列に並ぶ
```

**注意**: Close-Short は `side=Long` (買い戻し) になり, **best_bid に並ぶ**。
直感的に "売りに引っかけたい" と思いがちだが maker は反対側に立つ。

## AlgoParams

| key | 型 | デフォルト | 説明 |
|---|---|---|---|
| `max_total_ms` | u32 | `60000` | 全体 wall-clock budget。超過で abort |
| `repost_poll_ms` | u32 | `250` | book の poll 間隔 |
| `repost_threshold_ticks` | u32 | `0` | tick-aware 閾値 (※現状 placeholder, 0 = touch 変化で都度 repost) |
| `max_book_age_ms` | u32 | `500` | book.ts がこれより古ければ abort (0 で無効化) |
| `reduce_only` | bool | `false` | reduce_only flag |

> **`repost_threshold_ticks` について**: 80% プロト時点では tick size を per-symbol で持っていないため
> 実質 no-op。将来的に `executor-core::symbol` に tick metadata を入れた時点で有効化予定 (Gemini PR-4 deferred)。

## repost のタイミング

`current_quote = (cloid, px)` を保持し, 新 touch との差で判定:

```rust
let need_repost = match &current_quote {
    None => true,                                  // 初回 → 立てる
    Some((_, old_px)) => (new_touch - old_px).abs() > Decimal::ZERO,  // 任意の変化で repost
};
```

repost = **cancel 旧 + place 新** を **同じ BatchSender に enqueue**。
両者は次の 100 ms flush で 1 POST にまとまる。

## 終了条件

| 条件 | 結果 |
|---|---|
| `remaining <= 0` | `aborted=false`, full fill |
| `abort signal` | `aborted=true`, `abort_reason="aborted by caller"` |
| `started_instant.elapsed() >= max_total` | `aborted=true`, `abort_reason="max_total (...) elapsed with N remaining"` |

最終 quote は abort/timeout/完了いずれの経路でも cancel される (cleanup 漏れなし)。

## エラー

| エラー | 原因 |
|---|---|
| `passive: empty bids` / `passive: empty asks` | book にこちら側の touch が存在しない |
| `book stale (...)` | WS 切断中など |
| `derived size <= 0` | resolve_side_and_size の 0 サイズ |
| `batch enqueue: ...` | BatchSender flusher が dead (fatal) |

## ユースケース

### A. 100 BTC を maker で取得 (rebate 取りたい)

```json
{
  "algorithm":  "passive",
  "symbol":     "BTC",
  "intent":     "open",
  "target_size":"100.0",
  "params": {
    "max_total_ms":  3600000,
    "repost_poll_ms": 500,
    "max_book_age_ms": 1000
  }
}
```

最大 1 時間粘る。500ms 毎に板チェックして touch がずれたら cancel + repost。

### B. ポジション解消 (急がない)

```json
{
  "algorithm":  "passive",
  "symbol":     "BTC",
  "intent":     "close",
  "target_size":"0",
  "params": { "max_total_ms": 1800000 }
}
```

reduce_only=true を併用すると, ヘッジ用途で意図しない逆ポジ作成を防げる。

## テスト (`passive_follow::tests`)

9 ケース:

- 3 件: `touch_for_side` の long/short/empty
- 5 件: integration via MockHlClient + watcher task で fill 模擬
  - ALO at best_bid の検証 (price=49999)
  - 板移動時の cancel + repost (≥2 placed + ≥1 cancelled)
  - close-short が long で best_bid に並ぶ
  - abort 即時
  - max_total timeout (cancel される)
- 1 件: partial fill + book move → 残量で repost (PR-4 round 2 で追加)

## 関連

- [MARKET](market.md) — 同じ目的を taker で
- [TWAP](twap.md) — child_algo=passive で時間分散
- [MARKET_MAKE](market_make.md) — 両建て maker (target 中立に保つ)
