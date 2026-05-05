# `executor-cli`

Rust 製の REST クライアント。動作確認 / dry-run / 緊急停止に使う。

> 実装: [`executor/crates/executor-cli/src/main.rs`](../../executor/crates/executor-cli/src/main.rs)
> PR: [#65](https://github.com/howlrs/diff-old-new/pull/65)

## ビルド & 設置

```bash
cd executor
cargo build -p executor-cli --release
ls target/release/executor-cli   # → 実行可能ファイル

# (option) PATH に通す
ln -s "$(pwd)/target/release/executor-cli" ~/bin/executor-cli
```

cargo run でも可:

```bash
cargo run -p executor-cli -- <subcommand>
```

## グローバルオプション

| フラグ | 環境変数 | デフォルト | 説明 |
|---|---|---|---|
| `--url <URL>` | `EXECUTOR_URL` | `http://127.0.0.1:8085` | サーバ URL (末尾スラッシュなし) |

## サブコマンド

### `health`

```bash
executor-cli health
```

```json
{
  "status": "ok",
  "algorithms": ["market", "passive", "twap", "market_make"],
  "health": { ... },
  "running_executions": 0
}
```

### `positions`

```bash
executor-cli positions
```

### `book <symbol>`

```bash
executor-cli book BTC
```

### `exec`

```bash
executor-cli exec \
  --algo market \
  --symbol BTC \
  --intent open \
  --size 0.1 \
  --params '{"max_slippage_bps":"20","max_attempts":3}'
```

| フラグ | 必須 | 内容 |
|---|---|---|
| `--algo` | ✓ | `market` / `passive` / `twap` / `market_make` |
| `--symbol` | ✓ | symbol 名 |
| `--intent` | デフォルト `open` | `open` / `close` / `set_target` |
| `--size` | ✓ | string Decimal で送信 |
| `--params` | デフォルト `{}` | JSON object 文字列 |

JSON が malformed なら **process が即時 exit**。シェル引数の quote に注意。

```json
{ "exec_id": "0190bd51-f2cd-7e4f-...", "algorithm": "MARKET" }
```

### `status <id>`

```bash
executor-cli status 0190bd51-f2cd-7e4f-...
```

### `cancel <id>`

```bash
executor-cli cancel 0190bd51-f2cd-7e4f-...
```

### `emergency-stop`

```bash
executor-cli emergency-stop
```

> 注: `executor-cli` は現状 `X-Operator-ID` を送らない。
> auditability が必要な現場では `curl -H "X-Operator-ID: ..."` か Python connector ラッパを使う。

## ログ出力

`RUST_LOG=info executor-cli ...` で trace/debug を出せる。デフォルトは `warn`。

```bash
RUST_LOG=executor_cli=debug,reqwest=info executor-cli health
```

## エラー

サーバが 4xx/5xx を返すと `Error: HTTP <code>: <body>` で exit code 1。

```bash
$ executor-cli book BTC
Error: HTTP 404 Not Found: {"code":"not_found","message":"book for BTC"}
```

サーバ自体に到達できない (port 違い / 未起動) と reqwest 側のエラーが出る:

```bash
$ executor-cli health --url http://127.0.0.1:9999
Error: error sending request for url (http://127.0.0.1:9999/v1/health): error trying to connect: ...
```

## ユースケース

### A. 開発中の手早い動作確認

```bash
# 別 terminal で
EXECUTOR_BIND=127.0.0.1:8085 cargo run -p executor-server --release

# 確認
executor-cli health
executor-cli exec --algo market --symbol BTC --intent open --size 0.01
executor-cli status <exec_id>
```

### B. キルスイッチ (運用)

```bash
EXECUTOR_URL=https://exec.internal:8443 executor-cli emergency-stop
```

(本番では mTLS / Auth proxy を前段に入れる前提. 後述の operations/deployment.md 参照)

### C. CI から spawn して smoke

[`tests/test_executor_client_live.py`](../../tests/test_executor_client_live.py) と同様,
shell から `executor-server` を spawn → `executor-cli health` を polling して起動完了確認 →
本テスト, という pattern が組める。

## 関連

- [REST API リファレンス](api/rest.md)
- [Python connector](connector/python.md)
- [運用ガイド](operations/deployment.md)
