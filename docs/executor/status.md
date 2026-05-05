# 実装ステータス

最終更新: 2026-05-04 (PR-1 〜 PR-8 マージ完了時点)

## 全体進捗

| Phase | 状態 |
|---|---|
| **80% プロトタイプ** (PR-1〜PR-8) | ✅ **完了** (`develop` マージ済) |
| **Phase 3.5** (鍵管理 + Real signer + Real HL POST) | 未着手 (本ドキュメントの「残タスク」参照) |
| **Phase 4** (本番運用 + observability + Auth) | 未着手 |

## マージ済み PR (8 本)

| PR | 内容 | merge SHA |
|---|---|---|
| [#58](https://github.com/howlrs/diff-old-new/pull/58) | PR-1 Cargo workspace + executor-core types | `be1d043` |
| [#59](https://github.com/howlrs/diff-old-new/pull/59) | PR-2 executor-hl mock signer + WS state + TokenBucket + BatchSender | `2a87904` |
| [#60](https://github.com/howlrs/diff-old-new/pull/60) | PR-3 Algorithm trait + MARKET (taker IOC) | `8cdb748` |
| [#61](https://github.com/howlrs/diff-old-new/pull/61) | PR-4 PassiveFollowAlgorithm | `9b65830` |
| [#62](https://github.com/howlrs/diff-old-new/pull/62) | PR-5 TwapAlgorithm + helper consolidation | `0f84efb` |
| [#63](https://github.com/howlrs/diff-old-new/pull/63) | PR-6 MarketMakeAlgorithm | `52860cb` |
| [#64](https://github.com/howlrs/diff-old-new/pull/64) | PR-7 axum REST + WS server | `5c81329` |
| [#65](https://github.com/howlrs/diff-old-new/pull/65) | PR-8 emergency_stop + Python connector + e2e | `fcfd2ab` |

## 実装済みコンポーネント

### `executor-core` (22 tests)

- [x] `Cloid` (uuid v7 → 0x + 32 hex)
- [x] `Symbol` (HIP-3 dex 識別含む)
- [x] `Address` / `Side` / `Tif` / `OrderId` / `Fill`
- [x] `Intent` / `OrderIntent` / `CancelIntent` / `Progress` / `ExecutionReport`
- [x] `AlgoParams` (`get_decimal` / `get_u32` 含む)
- [x] `AppState` (book/position/open_orders/recent_fills/health/nonce 分割 lock)
- [x] `NonceManager` (atomic counter + 100 件 rolling window)
- [x] `AlgoError` / `ExecutorError`

### `executor-hl` (17 tests)

- [x] `Signer` trait + `MockSigner` (deterministic per-nonce 署名模擬)
- [x] `TokenBucket` rate limiter (millisecond-precision atomic CAS, HoL fix)
- [x] `BatchSender` (mpsc + 100ms flusher, order/cancel 分離, 個別 ack)
- [x] `HlClient` trait + `MockHlClient` (calls 記録, oid 模擬値, account/book seed)
- [x] `RealHlClient` 骨格 (signer + rate_limiter + reqwest, POST 本体は 80% プロト)
- [x] `WsStateManager` (split-lock state 更新, partial fill aggregation, cancel/reject)

### `executor-algo` (56 tests)

- [x] `Algorithm` trait + `ExecutionContext`
- [x] 共通 helpers: `drain_new_fills` / `taker_limit_price` / `ensure_book_fresh` / `collect_own_fills` / `build_report`
- [x] `MarketAlgorithm` (taker IOC + slippage cap + partial-fill loop)
- [x] `PassiveFollowAlgorithm` (maker ALO at touch + cancel/repost)
- [x] `TwapAlgorithm` (時間スライス, child=market/passive, 累積目標方式)
- [x] `MarketMakeAlgorithm` (target 駆動 2-sided ALO + inventory skew)

### `executor-server` (18 tests)

- [x] `ServerState` (Arc<AppState> + BatchSender + HlClient + Signer + Registry)
- [x] `OrderRouter` (name → Box<dyn Algorithm>)
- [x] `ExecutionRegistry` (HashMap<ExecutionId, Arc<RwLock<ExecutionHandle>>> + abort_all())
- [x] REST: health / positions / book / start / status / cancel / emergency_stop
- [x] WS: `/v1/exec/{id}/ws` (broadcast fan-out)
- [x] `ServerError` (HTTP マップ + Internal masking)
- [x] `ExecutionStatus::Finalizing` (race 対策中間 state)

### `executor-cli`

- [x] clap 4 derive, 7 サブコマンド
- [x] `EXECUTOR_URL` env, `--params` JSON 引数

### Python (`src/executor/`, 8 unit + 5 live tests)

- [x] `ExecutorClient` (httpx async, REST + WS)
- [x] `Algorithm` / `Intent` / `ExecutionStatus` (StrEnum, Rust serde 一致)
- [x] `ExecutorClientError` (status + body)
- [x] `tests/test_executor_client.py` (httpx.MockTransport で 8 ケース)
- [x] `tests/test_executor_client_live.py` (subprocess.Popen で 5 e2e)

## Gemini レビュー反映済み修正

| PR | 主な反映 |
|---|---|
| 3 | stale book detection / `tokio::time::Instant` for paused-time |
| 4 | partial-fill + book-move test |
| 5 | drain_new_fills / taker_limit_price / ensure_book_fresh を algorithm.rs に集約 |
| 6 | repost_bps_threshold=0 の warn / known limitations 明記 |
| 7 | Finalizing 中間 status / Internal error masking / abort_all() |
| 8 | cancel-before-abort 順序 / X-Operator-ID header |

各レビュー結果は SurrealDB `review_log` (タグ `pr-N,gemini-review`) に保存済。

## テスト集計

| 区分 | 件数 |
|---|---|
| Rust (executor-core) | 22 |
| Rust (executor-hl) | 17 |
| Rust (executor-algo) | 56 |
| Rust (executor-server unit) | 8 |
| Rust (executor-server integration) | 10 |
| **Rust 計** | **113** |
| Python (既存 audit/strategy/etc) | 71 |
| Python (executor connector) | 8 |
| **Python (CI 対象) 計** | **79** |
| Python (live e2e, marker live) | 5 |
| **総計** | **197** |

CI: `cargo fmt` / `cargo clippy -D warnings` / `ruff` / `mypy` / `scripts/check_ci_local.sh` 全クリーン。

---

## 残タスク (Phase 3.5 以降)

### 必須 (本番投入の前提)

| 項目 | 担当 | 備考 |
|---|---|---|
| 鍵管理ブレスト | TBD | agent wallet / master EOA / KMS / vault どれで鍵を持つか確定 |
| `Eip712AgentSigner` 実装 | executor-hl | `secrecy::Secret` + `zeroize`, HL の typed-data 構造に合わせる |
| `RealHlClient::place_orders` 完成 | executor-hl | POST `/exchange` payload + signature, レスポンス JSON parse |
| `RealHlClient::cancel_orders` 完成 | executor-hl | 同上 |
| Real WS subscriber | executor-hl | tokio-tungstenite で `wss://...`, 切断 + 5min reconcile |
| `clearinghouseState` 完全 parse | executor-hl | 80% プロトでは骨格のみ |

### 推奨 (本番品質)

| 項目 | 内容 |
|---|---|
| Auth レイヤ (mTLS / SSO proxy) | executor-server を public に晒す前に必須 |
| Observability | Prometheus exporter, Loki/Grafana, emergency_stop alert |
| `ExecutionRegistry` の TTL/prune | 24h+ 連続稼働で完了済 entry 蓄積する問題への対処 |
| `all_fills` の disk persist (MM) | 長時間 MM 用途。execution rotation で代替も可 |
| connector の `X-Operator-ID` 渡し | Python から audit ID を渡せるように |
| WS 再接続 / Lagged backoff (Python) | broadcast Lagged 後の自動再接続 |
| Pydantic schema 中央化 | Rust serde ↔ Python 自動同期 (Gemini PR-8 deferred) |

### 任意 (応用)

| 項目 | 内容 |
|---|---|
| `tick_size` per symbol meta | PASSIVE/MM の `repost_threshold_ticks` 有効化 |
| empty book backoff | 一時的な板スカに対する retry 戦略 (Gemini PR-4 deferred) |
| 複数 process 並列稼働 | agent wallet × algorithm 種別ごとに独立プロセス |
| MARKET の dynamic slippage | 直近 fill 結果で slippage を学習 |

## 設計ドキュメント

- 設計仕様 v2 (Gemini deep review 反映): [`../specs/2026-05-04-rust-executor-design.md`](../specs/2026-05-04-rust-executor-design.md)
- 完了サマリ (SurrealDB output_log, タグ `rust,executor,8-pr-series,complete,80-percent`):  
  `/home/o9oem/workspace/surreal-query.sh --search "8-pr-series complete"`

## 関連

- [README](README.md) — 全体目次
- [architecture](architecture.md) — 実装後の構造
- [deployment](operations/deployment.md) — 本番投入前チェックリスト
