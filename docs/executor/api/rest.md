# REST API リファレンス

base URL: `http://127.0.0.1:8085` (デフォルト. `EXECUTOR_BIND` 環境変数で変更可)

すべてのレスポンスは `Content-Type: application/json`。
エラーレスポンスは:

```json
{ "code": "<not_found|bad_request|conflict|internal>", "message": "<説明>" }
```

`internal` の場合, クライアントに渡す message は固定文字列 (`"internal server error"`).
詳細は server-side log のみに記録される (Gemini PR-7 セキュリティ指摘反映).

---

## エンドポイント一覧

| Method | Path | 用途 |
|---|---|---|
| GET | `/v1/health` | ヘルスチェック + アルゴリズム一覧 |
| GET | `/v1/positions` | 全シンボルの現在ポジション |
| GET | `/v1/book/{symbol}` | top-of-book スナップショット |
| POST | `/v1/exec` | execution 起動 |
| GET | `/v1/exec/{id}` | execution 状態 + 終了レポート |
| POST | `/v1/exec/{id}/cancel` | 個別 abort |
| POST | `/v1/emergency_stop` | キルスイッチ (全 cancel + 全 abort) |
| GET | `/v1/exec/{id}/ws` | WebSocket Progress ストリーム ([websocket.md](websocket.md)) |

---

## `GET /v1/health`

サーバが受付可能か, どのアルゴリズムをサポートしているか確認。

```bash
curl http://127.0.0.1:8085/v1/health
```

```json
{
  "status": "ok",
  "algorithms": ["market", "passive", "twap", "market_make"],
  "health": {
    "ws_connected": false,
    "last_book_update": null,
    "last_user_event": null,
    "last_reconciliation": null,
    "ws_reconnect_count": 0,
    "ws_message_count": 0
  },
  "running_executions": 0
}
```

| フィールド | 型 | 説明 |
|---|---|---|
| `status` | string | 常に `"ok"` (このフィールドの存在自体が起動完了を表す) |
| `algorithms` | string[] | OrderRouter が build できる名前 (snake_case) |
| `health` | object | WS 接続状態, 直近イベント時刻, reconnect 回数 |
| `running_executions` | u64 | ExecutionRegistry が保持する全 entry 数 (終了済み含む) |

---

## `GET /v1/positions`

全シンボルの現在ポジションを返す。

```bash
curl http://127.0.0.1:8085/v1/positions
```

```json
{
  "positions": {
    "BTC": {
      "size": "0.5",
      "entry_px": "49500.0",
      "unrealized_pnl": "100.0",
      "margin_used": "1000.0",
      "last_update": "2026-05-04T07:23:11.123Z"
    }
  }
}
```

`size` は **signed** (positive=long, negative=short). 精度保持のため string で送出。

---

## `GET /v1/book/{symbol}`

top-of-book スナップショット。symbol は `BTC` / `xyz:SP500` 等そのまま。

```bash
curl http://127.0.0.1:8085/v1/book/BTC
```

```json
{
  "bids": [{"px": "49999", "sz": "10", "n": 1}],
  "asks": [{"px": "50001", "sz": "10", "n": 1}],
  "ts":   "2026-05-04T07:23:11.123Z"
}
```

`ts` は WS 受信時刻。`null` の場合は WS 未接続/未配信。
`AppState::book` に該当 symbol が無ければ **404 not_found**。

---

## `POST /v1/exec`

execution を起動して `exec_id` を返す。

### リクエスト

```json
{
  "algorithm":   "market",
  "symbol":      "BTC",
  "intent":      "open",
  "target_size": "0.1",
  "params":      { "max_slippage_bps": "20" }
}
```

| フィールド | 型 | 説明 |
|---|---|---|
| `algorithm` | string | `market` / `passive` / `twap` / `market_make` (大文字小文字無視, 別名 `passive_follow` / `mm` 可) |
| `symbol` | string | `BTC` / `xyz:SP500` 等 |
| `intent` | enum | `open` / `close` / `set_target` (詳細は[各 algo doc](../algorithms/) 参照) |
| `target_size` | string→Decimal | f64 経由を避けるため文字列で送る。`"0.1"` 推奨 |
| `params` | object | アルゴリズム別パラメータ (各 algo doc 参照) |

### レスポンス (200 OK)

```json
{
  "exec_id":   "0190bd51-f2cd-7e4f-9b3a-89f5...",
  "algorithm": "MARKET"
}
```

`exec_id` は uuid v7 (時刻順序保持)。

### エラー

| status | code | 条件 |
|---|---|---|
| 400 | `bad_request` | `algorithm` が未対応 / target_size が不正 / params が algo 仕様違反 |

---

## `GET /v1/exec/{id}`

execution の状態と最終レポート。

```bash
curl http://127.0.0.1:8085/v1/exec/0190bd51-f2cd-7e4f-9b3a-89f5...
```

```json
{
  "exec_id":   "0190bd51-...",
  "algorithm": "MARKET",
  "status":    "completed",
  "report": {
    "exec_id":      "0190bd51-...",
    "algorithm":    "MARKET",
    "started_at":   "2026-05-04T07:23:11.000Z",
    "finished_at":  "2026-05-04T07:23:12.345Z",
    "target_size":  "0.1",
    "filled_size":  "0.1",
    "avg_px":       "50001.5",
    "total_fees":   "0.012",
    "fills": [
      {"symbol":"BTC","cloid":"0x019...","oid":1234,"side":"long",
       "px":"50001","sz":"0.05","fee":"0.006","ts":"..."},
      {"symbol":"BTC","cloid":"0x019...","oid":1235,"side":"long",
       "px":"50002","sz":"0.05","fee":"0.006","ts":"..."}
    ],
    "aborted": false,
    "abort_reason": null
  },
  "error": null
}
```

`status` の取りうる値:

| status | 意味 |
|---|---|
| `running` | 実行中 |
| `finalizing` | join handle が finished だが未だレポート抽出中 (Gemini PR-7 race 対策の中間 state) |
| `completed` | 完了 (`report.aborted == false`) |
| `aborted` | abort 経由で完了 (`report.aborted == true`) |
| `failed` | algo が `Err` を返した / panic 等 |

`finalizing` を見たクライアントは数十 ms 後に再度 GET すると確定状態が取れる。

### エラー

| status | code | 条件 |
|---|---|---|
| 400 | `bad_request` | `id` が UUID として parse 不可 |
| 404 | `not_found` | 該当 exec_id がレジストリに存在しない |

---

## `POST /v1/exec/{id}/cancel`

特定 execution を abort する。サーバは即座に 200 を返し, 実際の cancel 発信は次の BatchSender flush (≦100ms) で行われる。

```bash
curl -X POST http://127.0.0.1:8085/v1/exec/0190bd51-.../cancel
```

```json
{ "exec_id": "0190bd51-...", "abort_signaled": true }
```

`abort_signaled` は **abort watch channel に true を send した** ことを示す。
target_size に達した直後など, algo が abort より早く完了した場合でも `true` を返す (idempotent)。

---

## `POST /v1/emergency_stop`

**キルスイッチ**: 全 execution を abort + 全 open order を一括 cancel。

```bash
curl -X POST -H "X-Operator-ID: alice@desk" \
  http://127.0.0.1:8085/v1/emergency_stop
```

```json
{
  "aborted_executions": 3,
  "cancelled_orders":   12
}
```

### 仕様 (Gemini PR-8 review 反映)

1. **Step 1: cancel 先行**  
   `AppState::open_orders` を snapshot → 全件 `CancelIntent` を BatchSender に enqueue
2. **Step 2: abort 後置**  
   `ExecutionRegistry::abort_all()` で全 Running 状態の algo に abort 信号  
   - 順序を逆にすると, abort 信号到達前に algo が新規 order を enqueue する余地が残る

### `X-Operator-ID` ヘッダ

監査ログ用の操作者識別子。任意。指定すると `tracing::warn!` ログに `operator=...` で残る。
未指定時は `unknown` で記録。

```text
WARN executor_server::routes operator="alice@desk" aborted_executions=3 cancelled_orders=12 emergency_stop dispatched
```

実運用では Auth/SSO レイヤから取得した識別子をプロキシで設定する想定 (現状の executor 自体は認証なし)。

---

## エラーコード対応

| HTTP | code | 例 |
|---|---|---|
| 400 | `bad_request` | unknown algorithm, invalid uuid, AlgoError::InvalidParams |
| 404 | `not_found` | book / execution が存在しない |
| 409 | `conflict` | 同 exec_id 重複起動 (現状未発生だが ExecutorError::ExecutionAlreadyRunning が予約) |
| 500 | `internal` | unexpected. message は固定文字列で詳細は log のみ |

---

## 関連ドキュメント

- [WebSocket リファレンス](websocket.md)
- [Python connector](../connector/python.md)
- [executor-cli](../cli.md)
- [4 アルゴリズムの params](../algorithms/)
