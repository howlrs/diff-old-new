# アーキテクチャ概要

> 詳細な設計理由 (cancel 戦略, nonce 管理, RwLock 分割理由など) は
> [`../specs/2026-05-04-rust-executor-design.md`](../specs/2026-05-04-rust-executor-design.md) を参照。
> 本ドキュメントは「実装後の現状」を整理した俯瞰資料。

## Cargo workspace 構成 (5 crate)

```
executor/
├── Cargo.toml                       # workspace root
└── crates/
    ├── executor-core/               # types/state/intent/cloid/nonce — IO なし
    │   ├── cloid.rs                 # uuid v7 → 0x + 32 hex
    │   ├── errors.rs                # AlgoError / ExecutorError
    │   ├── intent.rs                # Intent / OrderIntent / Progress / ExecutionReport
    │   ├── nonce.rs                 # NonceManager (atomic + 100 件 rolling window)
    │   ├── state.rs                 # AppState (book/position/open_orders/fills/health 分割 lock)
    │   ├── symbol.rs                # Symbol — HIP-3 dex 識別
    │   └── types.rs                 # Address / Side / Tif / OrderId / Fill
    │
    ├── executor-hl/                 # HL 通信 + 鍵を扱う唯一の crate
    │   ├── batch_sender.rs          # mpsc + 100ms flusher (order/cancel 分離)
    │   ├── errors.rs                # HlError
    │   ├── hl_client.rs             # HlClient trait + MockHlClient + RealHlClient (骨格)
    │   ├── rate_limiter.rs          # TokenBucket (HL ~1200/min)
    │   ├── signer.rs                # Signer trait + MockSigner (実 EIP-712 は別 PR)
    │   └── ws_state.rs              # WsStateManager (split-lock 更新)
    │
    ├── executor-algo/               # Algorithm trait + 4 アルゴリズム + 共通 helpers
    │   ├── algorithm.rs             # Algorithm trait, ExecutionContext, helpers
    │   ├── market.rs                # MARKET (taker IOC + slippage cap)
    │   ├── passive_follow.rs        # PASSIVE_FOLLOW (maker ALO at touch)
    │   ├── twap.rs                  # TWAP (時間スライス)
    │   └── market_make.rs           # MARKET_MAKE (target 駆動 2-sided ALO)
    │
    ├── executor-server/             # axum REST + WS
    │   ├── lib.rs                   # build_app(state) → Router
    │   ├── error.rs                 # ServerError + axum IntoResponse
    │   ├── registry.rs              # ExecutionRegistry / ExecutionHandle
    │   ├── router.rs                # OrderRouter — name → Box<dyn Algorithm>
    │   ├── routes.rs                # REST handler 群
    │   ├── state.rs                 # ServerState (Arc<AppState> + ...)
    │   ├── ws.rs                    # /v1/exec/{id}/ws ハンドラ
    │   ├── main.rs                  # bin: 起動 + tracing 設定
    │   └── tests/integration_rest.rs  # tower::ServiceExt::oneshot e2e
    │
    └── executor-cli/                # 開発用 CLI (clap)
        └── main.rs                  # 7 サブコマンド (health/.../emergency-stop)
```

### crate 依存関係

```
executor-server ──▶ executor-algo ──▶ executor-hl ──▶ executor-core
                            │                  │
                            └────── executor-core ◄─┘

executor-cli ──▶ executor-core (型のみ依存)
```

`executor-hl` のみが HL ネットワーク + 鍵を触る。アルゴリズムは `BatchSender` 経由でしか発注できないため、
レート制御 / 100ms バッチ / cloid トラッキング が一律に強制される。

## データフロー: 1 回の execution が走るとき

```
[Python 戦略]
    │ HTTP POST /v1/exec
    │ {"algorithm":"twap","symbol":"BTC","intent":"open","target_size":"0.1",
    │  "params":{"slice_count":10,"total_duration_ms":60000}}
    ▼
[axum routes::start_exec]
    │ 1) validate_algorithm_name()
    │ 2) OrderRouter::build("twap") → Box<dyn Algorithm>
    │ 3) ExecutionId::new() (uuid v7)
    │ 4) watch::channel(false)        ─ abort 用
    │ 5) broadcast::channel<Progress>  ─ WS subscriber 用
    │ 6) mpsc::channel<Progress>       ─ algo → bridge task
    │ 7) ExecutionContext を組み立て tokio::spawn(algo.run(ctx))
    │ 8) ExecutionHandle 作成 → ExecutionRegistry に登録
    ▼
[TwapAlgorithm::run]  (async, 別タスク)
    │ for slice_idx in 1..=slice_count:
    │   * AppState.book を read_lock で snapshot
    │   * ensure_book_fresh()
    │   * resolve_side_and_size()
    │   * cloid 生成 → BatchSender::enqueue(Place(OrderIntent))
    │   * drain_new_fills() で Progress::SliceFilled 発行
    │   * tokio::time::sleep(interval)
    │ build_report(...) → return
    ▼
[executor-hl::batch_sender flusher]
    │ 100ms 毎に order_intents / cancel_intents を分離して
    │ HlClient::place_orders() / .cancel_orders() を呼ぶ
    ▼
[MockHlClient]
    │ 80% プロト: orders を Vec に記録, oid 模擬値返す
    │ Real (将来): EIP-712 署名 + POST /exchange
    ▼
[HL exchange]  (将来)
```

並行して:

```
[axum ws::progress_ws]
    │ /v1/exec/{id}/ws upgrade
    │ ExecutionHandle.progress.subscribe()
    │ broadcast::Receiver で algo の Progress を Text frame として転送
    │ Lagged → close (HFT 文脈で stale データを送らない)
```

```
[axum routes::cancel_exec]
    │ POST /v1/exec/{id}/cancel
    │ ExecutionHandle.abort.send(true)
    ▼
[Algorithm::run の loop]
    │ if *abort_rx.borrow() { 板上 order を CancelIntent で BatchSender 経由 cancel; return aborted=true }
```

## AppState の split-lock 戦略

単一 `RwLock` だと高頻度な book 更新 (write) が algo の read を starve させるため、
4 種類の lock を独立保持:

```rust
pub struct AppState {
    pub book:         Arc<RwLock<HashMap<Symbol, OrderBook>>>,
    pub position:     Arc<RwLock<HashMap<Symbol, Position>>>,
    pub open_orders:  Arc<RwLock<HashMap<Cloid, OpenOrder>>>,
    pub recent_fills: Arc<RwLock<VecDeque<Fill>>>,
    pub health:       Arc<RwLock<HealthStatus>>,
    pub nonce_mgr:    Arc<NonceManager>,
}
```

**append-only 不変条件**: `recent_fills` は `push_back` のみで成長する。
`drain_new_fills(.., last_seen_idx)` の usize index ロジックは
この append-only 性に依存する (`docs/executor/algorithms/*.md` 参照)。

## ExecutionRegistry: 実行中ジョブの管理

```rust
pub struct ExecutionRegistry {
    inner: RwLock<HashMap<ExecutionId, Arc<RwLock<ExecutionHandle>>>>,
}

pub struct ExecutionHandle {
    pub exec_id: ExecutionId,
    pub algorithm: String,
    pub abort: watch::Sender<bool>,           // /cancel + emergency_stop で true
    pub progress: broadcast::Sender<Progress>, // WS subscriber 用
    pub join: Option<JoinHandle<...>>,        // GET /v1/exec/{id} で finalize
    pub status: ExecutionStatus,              // Running/Finalizing/Completed/Aborted/Failed
    pub final_report: Option<ExecutionReport>,
    pub final_error: Option<String>,
}
```

`ExecutionStatus::Finalizing` は **GET /v1/exec/{id} の race 対策中間 state**。
join.take() → handle.await の隙に他 GET が入っても "ほぼ完了" を表現可。

## Cancel 戦略 (重要)

**HL の nonce 無効化は in-flight (未到達) request を弾くだけ. 板上の指値は cancel されない。**
従って:

1. 各 order に **cloid (16 byte client-generated)** を付与
2. cancel は `BatchSender::enqueue(OrderOrCancel::Cancel(CancelIntent { by_cloid: Some(c), .. }))`
3. `POST /v1/emergency_stop` は **Cancel-then-Abort** 順序:
   - Step 1: `AppState::open_orders` を snapshot し, 全件 CancelIntent を batch enqueue
   - Step 2: `ExecutionRegistry::abort_all()` で全 algo に abort 信号
   - 順序逆だと, abort 信号到達前に algo が新規 order を enqueue する余地がある (Gemini PR-8 指摘)

## Rate limit + Batch flush

- TokenBucket: HL ガイダンス ~1200 req/min - margin → 安全側で運用
- BatchSender: mpsc → 100ms tick で order/cancel を分離して `place_orders` / `cancel_orders` 一括送信
- `place_orders` と `cancel_orders` を別 batch にする理由: HL は ALO と IOC/GTC を別 batch 推奨

## Time handling: `tokio::time::Instant`

長時間 loop (PASSIVE_FOLLOW, TWAP, MARKET_MAKE) の deadline 計測は **必ず `tokio::time::Instant`** を使う。
`std::time::Instant` だと `tokio::time::pause()` 環境 (paused-time テスト) で
deadline が永遠に経過しない (PR-3 で Gemini 指摘)。

## 参照

- 設計仕様 v2: [`../specs/2026-05-04-rust-executor-design.md`](../specs/2026-05-04-rust-executor-design.md)
- 各 algo 実装ノート: [`algorithms/`](algorithms/)
- API 仕様: [`api/`](api/)
