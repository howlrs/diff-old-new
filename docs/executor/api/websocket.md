# WebSocket API リファレンス

execution の Progress イベントをリアルタイムでストリームするための単一エンドポイント。

```
GET /v1/exec/{id}/ws
Upgrade: websocket
```

各メッセージは **Text frame** (`Message::Text`) で, JSON シリアライズされた `Progress` enum。
複数 subscriber が同じ exec_id を購読可能 (broadcast fan-out)。

---

## 接続

### `wscat` で確認

```bash
# 起動済み executor-server に対して
wscat -c "ws://127.0.0.1:8085/v1/exec/0190bd51-f2cd-7e4f-9b3a-.../ws"
```

### Python 連携

```python
from src.executor import ExecutorClient

async with ExecutorClient("http://127.0.0.1:8085") as cli:
    resp = await cli.start(algorithm="market", symbol="BTC", intent="open", target_size="0.1")
    async for evt in cli.stream(resp.exec_id):
        print(evt)
```

詳細は [`../connector/python.md`](../connector/python.md)。

---

## メッセージスキーマ (Progress)

Rust 側の `executor_core::intent::Progress` enum を `serde(tag = "type", rename_all = "snake_case")` で
シリアライズしたもの。`type` フィールドで discriminate する。

### 1. `started` — execution 開始

```json
{
  "type":    "started",
  "exec_id": "0190bd51-...",
  "ts":      "2026-05-04T07:23:11.000Z"
}
```

algo の `run()` 冒頭で 1 回だけ送出。

---

### 2. `slice_filled` — 1 つの fill が届いた

```json
{
  "type":              "slice_filled",
  "slice":             1,
  "cloid":             "0x019defde36677fa287de0871c9f231a5",
  "px":                "50001",
  "sz":                "0.05",
  "cumulative_filled": "0.05"
}
```

| フィールド | 意味 |
|---|---|
| `slice` | アルゴリズム内の slice index (TWAP は 1〜slice_count, MARKET は試行回数, PASSIVE / MM は 0 固定) |
| `cloid` | この fill に紐づく cloid (`0x` + 32 hex) |
| `px` | 約定価格 (string Decimal) |
| `sz` | 約定サイズ (string Decimal) |
| `cumulative_filled` | この event 終了時点の累積約定サイズ |

---

### 3. `heartbeat` — 進捗ハートビート

```json
{
  "type":              "heartbeat",
  "cumulative_filled": "0.05",
  "remaining":         "0.05",
  "ts":                "2026-05-04T07:23:11.500Z"
}
```

長時間稼働の algo (TWAP, MARKET_MAKE) が slice 間/repost loop 間で送出。
クライアントが切断検知に使える (一定時間届かなければ再接続)。

---

### 4. `aborted` — 途中停止

```json
{
  "type":   "aborted",
  "reason": "aborted by caller",
  "ts":     "2026-05-04T07:23:12.000Z"
}
```

> 注: 現実装では `aborted` Progress 自体は明示送出していない。
> 終了は `Completed` (with `report.aborted=true`) で表現される。
> `aborted` は将来送出が想定される予約 variant。

---

### 5. `completed` — 正常完了 / 完了

```json
{
  "type":         "completed",
  "filled_size":  "0.1",
  "avg_px":       "50001.5",
  "total_fees":   "0.012",
  "n_fills":      2,
  "ts":           "2026-05-04T07:23:12.345Z"
}
```

algo の最後に必ず 1 回送出される。`avg_px` は `Σ(px*sz) / Σsz`。

その後サーバは broadcast channel をクローズしないため (executor-server lifetime 中は維持),
完了後に新規 subscriber が繋いだ場合は何も流れない (REST GET で final report を取得すべき)。

---

### 6. `error` — 異常

```json
{
  "type":    "error",
  "message": "...",
  "ts":      "..."
}
```

将来の拡張用。現実装では Algorithm が `Err` を返した場合 Progress には流さず,
`/v1/exec/{id}` の `status=failed` + `error` フィールドで通知する。

---

## 切断条件

サーバ側から close するタイミングは以下:

| 条件 | 挙動 |
|---|---|
| クライアントが `Close` frame 送信 | 即座に close |
| broadcast channel が `Lagged(n)` を返した | 警告ログを出して close (HFT で stale を流さない方針) |
| broadcast channel が `Closed` (sender drop) | close |

`Lagged` 時のクライアント実装ガイド:
- 再接続して REST `/v1/exec/{id}` で現在の累積約定を取得
- 過去イベントは再送されない (broadcast の性質)

---

## バックプレッシャ / バッファ容量

`broadcast::channel(256)` で生成。長期 MM などで 256 件溜まると新規 event は古いものを上書き、
slow consumer は `Lagged` で蹴られる。

将来課題 (Gemini PR-7 指摘):
- 高頻度 MM 用に容量を 1024 等に増やす検討
- クライアント側の指数バックオフ再接続 (現状未実装. Python connector でも要追加)

---

## エラー

`/v1/exec/{id}/ws` 自体のエラーは upgrade 前に HTTP として返る:

| status | code | 条件 |
|---|---|---|
| 400 | `bad_request` | `id` が UUID として parse 不可 |
| 404 | `not_found` | 該当 exec_id が無い |

upgrade 後はエラー応答できないため, 上記のような異常が起きた場合は close で通知される。

---

## サンプル: 1 execution の完全な event 列

MARKET algo で `target_size=0.1`, slice 1 で full fill した場合:

```json
{"type":"started","exec_id":"...","ts":"2026-05-04T07:23:11.000Z"}
{"type":"slice_filled","slice":1,"cloid":"0x...","px":"50001","sz":"0.1","cumulative_filled":"0.1"}
{"type":"completed","filled_size":"0.1","avg_px":"50001","total_fees":"0.012","n_fills":1,"ts":"..."}
```

PASSIVE_FOLLOW で 2 回 partial fill した場合:

```json
{"type":"started",...}
{"type":"slice_filled","slice":0,"cloid":"0x...A","px":"49999","sz":"0.06","cumulative_filled":"0.06"}
{"type":"slice_filled","slice":0,"cloid":"0x...B","px":"50000","sz":"0.04","cumulative_filled":"0.10"}
{"type":"completed","filled_size":"0.10","avg_px":"49999.6","total_fees":"...","n_fills":2,"ts":"..."}
```

---

## 関連

- [REST リファレンス](rest.md)
- [Python connector](../connector/python.md) — `ExecutorClient.stream(exec_id)`
