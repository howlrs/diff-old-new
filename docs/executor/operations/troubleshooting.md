# Troubleshooting

開発・運用中によく遭遇するエラーと対処。

## ビルド系

### 1. `cargo build` で `could not find Cargo.toml`

```text
error: could not find `Cargo.toml` in `/home/.../diff-old-new` or any parent directory
```

**原因**: workspace ルートは `executor/` 配下。  
**対処**: `cd executor && cargo build`。 `executor/` の中で操作する。

### 2. `cargo clippy -D warnings` が `unused_mut` で落ちる

dev で書いたコードを `cargo clippy` に通したら lint で蹴られる。  
**対処**: 警告メッセージに従い `mut` 削除 / 不要 import 削除。CI は `-D warnings` 強制なので
ローカルで一度通しておく:

```bash
cd executor
cargo clippy --workspace --all-targets -- -D warnings
```

### 3. `expect_used` clippy エラー

`#![forbid(unsafe_code)]` + workspace lints で `unwrap_used` / `expect_used` が `warn`。
production コードで使うと CI で fail.

**対処**:
- production: `Result<_, _>` で適切にエラーを伝播
- test 内: モジュール冒頭に `#![allow(clippy::unwrap_used, clippy::expect_used)]` を入れる

## サーバ起動系

### 4. `executor-server` が即終了する

```text
Error: Address already in use (os error 98)
```

**原因**: port 8085 が他プロセス使用中。  
**対処**: `EXECUTOR_BIND=127.0.0.1:9999 cargo run -p executor-server` で port 変更, または `lsof -i :8085` で衝突プロセス特定。

### 5. systemd で起動するが動作しない

journalctl で `tracing` ログを確認:

```bash
journalctl -u executor-server -f
```

最初に `executor-server listening` が出ればOK。出ていない場合は前段のセットアップで panic している (signer 初期化失敗等)。

## アルゴリズム実行系

### 6. `market: empty asks (no best ask)` が頻発する

**原因**: `AppState.book` に該当 symbol のデータが無い。WS subscriber が未実装 (80% プロト) なため,
本番では Real WS が必要。テストでは `tests/integration_rest.rs:build_state_with_seed()` のように手動シード。

**対処 (開発時)**: テスト時は AppState に手動で seed する。または algo 起動前に
`HlClient::fetch_book_snapshot()` を呼んで AppState に書き込む helper を追加。

### 7. `book stale (XXXms > YYYms)` が頻発する

**原因**:
- WS が切断されている (本番では Real WS subscriber が再接続するまで)
- 開発時 `tokio::time::pause()` 環境で Utc::now() のみ進んでいる

**対処**:
- 本番: WS 再接続を待つ. または `max_book_age_ms` を緩める (デフォルト 500ms)
- テスト: `max_book_age_ms: 0` で freshness check を無効化

### 8. MARKET algo が `max_attempts (5) exceeded with X.XX remaining` で aborted

**原因**: IOC 投入後 `slice_timeout_ms` 内に fill が来ない. 板薄 / slippage 不足。

**対処**:
- `max_slippage_bps` を増やす
- `slice_timeout_ms` を増やす
- `max_attempts` を増やす
- それでも fill されないなら板情報が古い疑い

### 9. PASSIVE_FOLLOW が `max_total elapsed with N remaining` になる

**原因**: maker order に touch しなかった. または板移動が頻繁すぎてキャンセルが間に合わない。

**対処**:
- `max_total_ms` を伸ばす
- `child_algo: market` の TWAP に切り替える (確実に約定したい場合)
- `repost_poll_ms` を短くする (ただしレート制限注意)

### 10. MARKET_MAKE で `repost_bps_threshold = 0` 警告

```text
WARN executor_algo::market_make repost_bps_threshold = 0 triggers reposting on every mid change ...
```

**原因**: 設定値が小さすぎる。  
**対処**: 本番では `≥1` (1bps) を推奨。0 は test 用。

## Python connector

### 11. `RuntimeError: ExecutorClient must be used as an async context manager`

**原因**: `cli = ExecutorClient(url); await cli.health()` のように context manager 外で呼んだ。  
**対処**: `async with ExecutorClient(url) as cli:` で囲む。

### 12. `cli.stream(id)` で `RuntimeError: requires 'websockets'`

**原因**: `websockets` package が import できない。  
**対処**: `pip install -e ".[dev,all]"` で base 依存に含まれているはず。venv 確認。

### 13. `ExecutorClientError(404, ...)` で `not_found`

**原因**: exec_id が存在しない / book 未シード。  
**対処**: 直前の `start()` の戻り値の `exec_id` を確認。typo していないか。

## live e2e テスト

### 14. `executor-server binary not found` で skip される

```text
SKIPPED [1] executor-server binary not found — run `cargo build -p executor-server` first
```

**原因**: バイナリ未ビルド。  
**対処**: `cd executor && cargo build -p executor-server`。release でも debug でも fixture が拾う。

### 15. `executor-server did not start in time`

**原因**: 起動が 10 秒以内に /v1/health に応答しない。
- TLS 周りのライブラリ build が走った直後だと起こる場合あり
- マシン負荷高い

**対処**:
- 一度 `cargo run -p executor-server` を手で叩いて起動を見る
- fixture の deadline を引き上げる (`tests/test_executor_client_live.py:69`)

## CI

### 16. CI の Python 部分で extras 不足

```text
ModuleNotFoundError: No module named 'altair'
```

**対処**: 既に `pip install -e ".[dev,all]"` で全 extras を入れる方針が CI に入っている。
新しい extras 追加時は `pyproject.toml` の `all` meta extra にも反映 (R-recently issue で痛い目見た案件)。

### 17. CI Rust 部分が `cargo test --all-features` で fail

ローカル `cargo test --workspace` (all-features 無し) では通るのに CI で失敗するときは
feature flag 違いの可能性。

**対処**: ローカルで `cargo test --workspace --all-features` を再現。
`scripts/check_ci_local.sh` が exact CI と同じコマンドを叩く。

## 関連

- [dev-setup](dev-setup.md) — 環境セットアップ
- [REST API](../api/rest.md) — エラーレスポンス仕様
- [各 algo doc](../algorithms/) — algorithm 固有エラーと対処
