# MARKET アルゴリズム

> 実装: [`executor/crates/executor-algo/src/market.rs`](../../../executor/crates/executor-algo/src/market.rs)
> PR: [#60](https://github.com/howlrs/diff-old-new/pull/60)

## 役割

**taker (テイカー) スタイルの即時執行**。slippage 上限つきの IOC (Immediate-or-Cancel) 指値を投入し,
部分約定時は残量で再投入。`max_attempts` 回試行してもまだ残量がある場合は abort。

ユーザー要望「現在の A ポジションを即座に解除したい」「成り行きで B 銘柄を 100 取りたい」に対応する基本アルゴリズム。

## 動作フロー

```
1. ctx.params から MarketParams を抽出 (validation 含む)
2. AppState.position から現在ポジション snapshot
3. resolve_side_and_size(intent, target_size, current) で side + abs_size 算出
4. while remaining > 0:
   a. abort 信号確認
   b. attempts > max_attempts なら abort 終了
   c. AppState.book から book snapshot
   d. ensure_book_fresh() で stale 検出 (max_book_age_ms)
   e. taker_limit_price() で slippage 込み limit_px 算出
   f. 新 cloid 生成 → BatchSender.enqueue(Place(IOC OrderIntent))
   g. collect_own_fills() で slice_timeout_ms まで fill 待機
   h. 新規 fill を all_fills に追加, remaining 減算
5. build_report() → Progress::Completed 送出
```

## Intent と side / size の決定

`resolve_side_and_size(intent, target_size, current_size)` の挙動:

| Intent | target_size の符号 | 動作 |
|---|---|---|
| `Open` | + | side=Long, abs_size = `|target_size|` |
| `Open` | - | side=Short, abs_size = `|target_size|` |
| `Open` | 0 | **エラー** (`InvalidParams: Open with target_size = 0`) |
| `SetTarget` | delta = target − current ≠ 0 | side = sign(delta), abs_size = `|delta|` |
| `SetTarget` | delta = 0 | **エラー** (`InvalidParams: already at target`) |
| `Close` | (current = 0) | **エラー** (`InvalidParams: no position to close`) |
| `Close` | target_size = 0 | side = 反対側, abs_size = `|current|` (全クローズ) |
| `Close` | target_size > 0 | side = 反対側, abs_size = `min(target_size, |current|)` (部分クローズ) |

> **Close + target_size > 0 のセマンティクス**: 「current の符号と逆方向に target_size 分まで動かす (上限 |current|)」。
> 例: current=-2.0, target_size=1.0 → side=Long, abs_size=1.0 (短ポジを 1.0 だけ買い戻し → 残 short 1.0)。

## AlgoParams

| key | 型 | デフォルト | 説明 |
|---|---|---|---|
| `max_slippage_bps` | string→Decimal | `"20"` | IOC 指値の slippage cap (basis points) |
| `max_attempts` | u32 | `5` | slice 試行の最大回数。超過で aborted=true で終了 |
| `slice_timeout_ms` | u32 | `1500` | 1 slice あたりの fill 待機 deadline |
| `max_book_age_ms` | u32 | `500` | book.ts がこれより古ければ stale としてエラー (`0` で無効化) |
| `reduce_only` | bool | `false` | OrderIntent の reduce_only フラグ |

`max_book_age_ms = 0` は `tokio::time::pause()` を使う unit test 用。実運用では必ず正の値。

## limit price 算出 (`taker_limit_price`)

```
Long (買いたい):  limit_px = best_ask * (1 + max_slippage_bps / 10000)
Short (売りたい): limit_px = best_bid * (1 - max_slippage_bps / 10000)
```

例: best_ask = 100, slippage = 50bps → limit_px = 100.5。
HL は IOC で limit を超えなければ部分約定 / 不成立を返す。

## 終了条件

| 条件 | 結果 |
|---|---|
| `remaining == 0` | `aborted=false`, full fill |
| abort signal | `aborted=true`, `abort_reason="aborted by caller"` |
| `attempts > max_attempts` | `aborted=true`, `abort_reason="max_attempts (N) exceeded with M remaining"` |

## ライブで起きうるエラー

| エラーメッセージ | 原因 | 対応 |
|---|---|---|
| `market: empty asks (no best ask)` | book に asks 行が無い | symbol 名 typo / WS 未接続 |
| `market: empty bids (no best bid)` | book に bids 行が無い | 同上 |
| `market: book stale (XXXms > YYYms)` | book.ts が古い | WS 再接続待ち / `max_book_age_ms` 緩和 |
| `Open with target_size = 0` | 仕様エラー | クライアント側を修正 |
| `derived size <= 0` | 内部矛盾 (通常出ない) | バグ報告 |

## ユースケース別パラメータ例

### A. 全クローズを最速で

```json
{
  "algorithm":  "market",
  "symbol":     "BTC",
  "intent":     "close",
  "target_size":"0",
  "params": { "max_slippage_bps": "30", "max_attempts": 3, "slice_timeout_ms": 1000 }
}
```

`reduce_only:true` を加えると, ヘッジ建て時の意図しない逆ポジ防止に。

### B. 100 BTC を素早く新規 long

```json
{
  "algorithm":  "market",
  "symbol":     "BTC",
  "intent":     "open",
  "target_size":"100.0",
  "params": { "max_slippage_bps": "50", "max_attempts": 10, "slice_timeout_ms": 2000 }
}
```

slippage 緩めて max_attempts 多めに。

## テスト (`market::tests`)

20 ケース:

- 7 件: side resolution (`resolve_side_open_long` 等)
- 2 件: limit price 計算 (long/short slippage)
- 4 件: book freshness (stale/recent/none/missing-ts)
- 2 件: full fill via Mock (open long / close short)
- 2 件: abort 経路 (signal / max_attempts)
- 2 件: partial fill → top-up (60% → remainder, 50% → 50% → full の chain)

`tokio::time::pause()` + `tokio::time::advance()` で実時間に依存しない。
`MockHlClient::placed_calls()` で発信内容を検証。

## 関連

- [Algorithm trait + ExecutionContext](../architecture.md)
- [REST `POST /v1/exec`](../api/rest.md)
- [PASSIVE_FOLLOW](passive_follow.md) — taker でなく maker で同じ目的
- [TWAP](twap.md) — 時間分散して MARKET / PASSIVE を呼ぶ
