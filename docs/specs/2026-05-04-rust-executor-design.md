# Rust Executor Design v2 — Hyperliquid 発注実行レイヤ

作成日: 2026-05-04
バージョン: v2.0 (Gemini deep review 反映後)
状態: Phase 3 実装着手準備

## v2 改訂点 (Gemini partner deep review 2026-05-04 反映)

| 区分 | 指摘 | 対応 |
|---|---|---|
| Bug | nonce 無効化は in-flight のみ. 板上の指値は cancel されない | emergency_stop = 全 cloid 一括 cancel + agent wallet deregister. nonce 無効化に頼らない |
| Bug | WS のみは状態乖離リスク | 起動 + WS 再接続 + 定期 5min で REST `clearinghouseState` reconciliation |
| Bug | 単一 RwLock でレイテンシスパイク | book / position / open_orders を別 lock + actor model (tokio mpsc) |
| Imp | cloid 活用 | 16-byte client-generated cloid を全 order に付与 |
| Imp | batch sender 専用 task | mpsc キュー + 0.1s flusher task |
| Imp | f64 精度問題 (sign 失敗) | 金融計算は `rust_decimal`, API 通信は string |
| Sec | rate limit 保護 | Token Bucket (1200/min - margin) を executor-hl に組み込み |
| Sec | 秘密鍵 zeroize | `secrecy` crate 採用 |

---

## 0. ビジョン

Hyperliquid HIP-3 (Trade[XYZ]) 米株 perp 戦略の **発注実行を Rust で実装** する.
責務単一の原則を「**各アルゴリズムが独立した発注実行ユニット**」「**executor 全体は HL への発注実行のみ**」の二層で守る.

戦略 (Python) と executor (Rust) は **REST + WebSocket** で通信し,
高水準 intent (「100 long にして」「TWAP で解除して」) を Python から受け,
Rust は内部状態 (板/ポジション/open orders) を保持しながら最速経路で HL に発注する.

---

## 1. 確定済み制約 (本ブレストでの決定事項)

| 項目 | 決定 |
|---|---|
| 構成 | **B Stateful Executor** (local book + 内部アルゴ実行) |
| 環境 | Linux local 開発 + VPS 本番前提 |
| 鍵管理 | testnet → mainnet 小額 → KMS の段階移行 |
| Cancel 戦略 | 通常時は個別 cancel (アルゴが自身の order id 管理), 緊急時のみ nonce 無効化 |
| 通信方式 | REST (指示/abort) + WebSocket (進捗 push) |

---

## 2. HL 最速経路の前提 (公式情報ベース)

### 2.1 公式 latency 数値
- co-located client: median 0.2 sec, p99 0.9 sec
- HyperBFT finality: 70 ms (これより速い確認手段は存在しない)

### 2.2 最速にするための要素
| 層 | 推奨 |
|---|---|
| ネットワーク | HL ノードに地理的に近い VPS (NY / Frankfurt / Tokyo) |
| 接続 | keep-alive HTTPS (TLS handshake 削減) |
| データ | WS から自前で板/状態を構築 (REST は使わない) |
| Batch | 0.1 秒ごとに order/cancel を batch 送信 |
| API wallet | agent wallet を 1 プロセスに 1 つ (master を露出させない) |
| Nonce | atomic counter で current ms へ fast-forward, rolling window 100 個活用 |
| Cancel 高速化 | nonce 無効化で一括 cancel (緊急時のみ) |
| WS 発注 | **存在しない** (REST POST `/exchange` のみ) |

### 2.3 制約
- WS 経由の発注は HL に存在しない
- HyperBFT finality 70 ms より速い確認は不可
- 「co-location」(物理隣接) は HL に該当する概念なし, validator network への RTT が支配

---

## 3. リポジトリ構成 (Cargo workspace)

```
diff-old-new/
├── pyproject.toml           ← Python 既存
├── src/                     ← Python L1/L2/L3 strategy
├── notebooks/
├── executor/                ← 新規 Rust workspace ルート
│   ├── Cargo.toml
│   ├── crates/
│   │   ├── executor-core/       lib (state, types, errors)
│   │   ├── executor-hl/         lib (HL HTTP/WS client, signer)
│   │   ├── executor-algo/       lib (Algorithm trait + 各アルゴ)
│   │   ├── executor-server/     bin (axum REST + WS server)
│   │   └── executor-cli/        bin (CLI test client, dry-run)
│   └── tests/                   integration tests
└── docs/specs/
    └── 2026-05-04-rust-executor-design.md
```

### 3.1 各クレートの単一責務
- **executor-core**: 純粋ロジック (state, types, errors). 外部 IO ゼロ
- **executor-hl**: HL 通信のみ (HTTP keep-alive, WS, EIP-712 sign, nonce mgr)
- **executor-algo**: 各アルゴ (1 アルゴ = 1 module = 1 trait impl)
- **executor-server**: REST + WS 配線のみ
- **executor-cli**: 開発時テスト用 CLI

---

## 4. 内部アーキテクチャ

```
┌────────────────────────────────────────┐
│      executor-server (axum)             │
│  POST /execute   POST /abort/{id}       │
│  GET  /ws/progress  GET  /health        │
│  POST /emergency_stop                    │
└──────────────┬──────────────────────────┘
               │
┌──────────────▼──────────────────────────┐
│     OrderRouter (executor-core)          │
│  - ExecutionId 管理                       │
│  - progress channel (tokio broadcast)    │
│  - abort signal                           │
└─┬──────────┬─────────────┬───────────────┘
  │          │             │
  ▼          ▼             ▼
┌────┐  ┌───────┐  ┌──────────┐
│TWAP│  │MARKET │  │ PASSIVE  │   各 Algorithm trait 実装
│Algo│  │Algo   │  │ FOLLOW   │   (executor-algo)
└─┬──┘  └───┬───┘  └────┬─────┘
  └────────┬┴───────────┘
           │
┌──────────▼──────────────────────────────┐
│      HL Client (executor-hl)             │
│  - State (book, position, orders, fills) │
│  - Order send (sign + http)              │
│  - Cancel                                 │
│  - Nonce manager                          │
└──────────┬──────────────────────────────┘
           │
┌──────────▼──────────────────────────────┐
│   Hyperliquid mainnet/testnet            │
│   - WS: book, userEvents                  │
│   - REST: /exchange (orders)              │
└─────────────────────────────────────────┘
```

### 4.1 設計原則
- 各 Algo は `Algorithm` trait を実装: `async fn run(&mut self, ctx: ExecutionContext) -> Result<ExecutionReport>`
- アルゴは **HLClient を借りる** だけ. アルゴ自身は HTTP/署名を知らない
- アルゴ追加 = ファイル 1 つ追加 (他コードに変更ゼロ) — OCP (Open/Closed)
- **Signer は trait で抽象化**. 80%プロトでは MockSigner を使い, 鍵不要で全体動作確認

---

## 5. Algorithm trait (Gemini v2: rust_decimal + lock 分割)

```rust
use rust_decimal::Decimal;

#[async_trait]
pub trait Algorithm: Send + Sync {
    /// アルゴ名 (logging 用)
    fn name(&self) -> &'static str;

    /// 実行. ctx で abort signal / progress channel / state を受け取る
    async fn run(&mut self, ctx: ExecutionContext) -> Result<ExecutionReport, AlgoError>;
}

pub struct ExecutionContext {
    pub exec_id: ExecutionId,
    pub symbol: Symbol,
    pub intent: Intent,                  // open / close / set_target
    pub target_size: Decimal,             // signed (rust_decimal: 精度 28 桁)
    pub params: AlgoParams,               // 各アルゴ固有
    pub timeout_sec: u32,
    pub fallback_to_taker: bool,
    pub progress_tx: broadcast::Sender<Progress>,
    pub abort_rx: oneshot::Receiver<AbortReason>,
    pub state: Arc<AppState>,             // 分割 lock (§6 参照)
    pub batch_tx: mpsc::UnboundedSender<OrderIntent>,  // batch sender への enqueue
}
```

### 5.1 数値型ポリシー
- 金融計算 (size / px / pnl): **`rust_decimal::Decimal`**
- API JSON ↔ 内部: `Decimal::from_str(&s)` / `decimal.to_string()`
- f64 は **使わない** (EIP-712 sign での丸め誤差で "Invalid Signature" エラーが出る既知の問題)

### 5.1 MVP アルゴ 4 種

| Algorithm | 動作 |
|---|---|
| **MARKET** | IoC で best price を taker 即時約定. 残量があれば再帰追加 (slippage cap まで) |
| **PASSIVE_FOLLOW** | best bid/ask に joined ALO 指値. tick で価格更新時に cancel→再発注 (個別 cancel) |
| **TWAP** | duration を slice_count で等分, 各 slice で MARKET or PASSIVE_FOLLOW を内部呼出 |
| **MARKET_MAKE** | 両側 ALO 指値継続. 在庫が target に近づいたら片側を緩める. cancel→再発注は個別. target 達成型 |

### 5.2 アルゴ間合成
- TWAP は MARKET / PASSIVE_FOLLOW を **内部呼び出し** (composition over inheritance)
- MARKET_MAKE は target 達成で停止 (定常 MM ではなく target 達成型)

---

## 6. State Engine (Gemini v2 反映: lock 分割 + reconciliation)

**Gemini 指摘**: 単一 `Arc<RwLock<LocalState>>` は WS 板更新 (write) がアルゴ参照 (read) を頻繁にブロック → レイテンシスパイク.

### 6.1 構造 (lock 分割)
```rust
pub struct AppState {
    pub book: Arc<RwLock<HashMap<Symbol, OrderBook>>>,        // 高頻度 write (WS)
    pub position: Arc<RwLock<HashMap<Symbol, Position>>>,      // 中頻度 write
    pub open_orders: Arc<RwLock<HashMap<Cloid, OpenOrder>>>,   // 中頻度 write
    pub recent_fills: Arc<RwLock<VecDeque<Fill>>>,             // 中頻度 write
    pub nonce_mgr: Arc<NonceManager>,                          // atomic counter
    pub health: Arc<RwLock<HealthStatus>>,                     // 低頻度 write
    pub rate_limiter: Arc<TokenBucket>,                        // 内部 atomic
}
```

ロックを分割することで:
- WS 板更新 (book lock) はアルゴの position/orders 参照をブロックしない
- アルゴが価格更新確認しても, 同時に他アルゴの fill 受信ブロックしない

### 6.2 さらなる最適化 (Phase 4 検討)
- `arc-swap` でロックフリー read 可能化
- actor model (mpsc) で書き込みは1 task に直列化, 読み手は snapshot 配信

### 6.3 更新源
- WS `l2Book` → book 更新
- WS `userEvents` → fills, position, open_orders 削除
- 発注成功 → open_orders 追加 (cloid キー)
- 起動 + WS 再接続 + 定期 5 分: REST `/info clearinghouseState` で **reconciliation**
- 5 分間隔 heartbeat → health 更新

### 6.4 Reconciliation (Gemini v2 追加)
WS のみに依存すると, 切断中の取引所側変更を取りこぼす. 以下のタイミングで REST 同期:
- 起動時: 一度フル取得 + open_orders 全更新
- WS 再接続時: フル取得 + 差分検出 → ローカル open_orders と乖離があれば warning + reconcile
- 定期 5 分: 軽量 sanity check (position 数値だけ照合)
- 緊急 stop 後: フル取得して未約定がゼロ確認

---

## 7. Nonce Manager + Batch Sender (Gemini v2 反映)

### 7.1 Nonce Manager
```rust
pub struct NonceManager {
    /// 次に発行する nonce 候補. current ms に fast-forward する
    counter: AtomicU64,
    /// 直近 100 個の rolling window. 監視のみ (HL 側が判定)
    window: ArrayQueue<u64>,
}

impl NonceManager {
    /// 次の nonce を発行. current ms より大きい値を保証
    /// 並列発注時の衝突なしのため atomic CAS
    pub fn next(&self) -> u64;

    /// rolling window 残量 (1 秒未満で 100 個埋まると後続が拒否される)
    pub fn remaining_capacity(&self) -> usize;
}
```

**注意**: 当初 v1 で「nonce 無効化 = 一括 cancel」と誤認していた.
v2 では nonce 無効化は使わず, 通常の monotonic 発行のみ.

### 7.2 Batch Sender (新規)
**Gemini 指摘**: アルゴが直接 `HLClient.place_order()` を呼ぶ設計だと, HL 推奨の "0.1 秒 batch" にならない.

```rust
pub struct BatchSender {
    pending_orders: mpsc::UnboundedReceiver<OrderIntent>,
    pending_cancels: mpsc::UnboundedReceiver<CancelIntent>,
    flush_interval: Duration,  // 100 ms
    rate_limiter: Arc<TokenBucket>,
}

// アルゴ側からは:
//   batch_sender_tx.send(OrderIntent { ... })?;  // non-blocking enqueue
// 内部 task が 100 ms ごとに pending を集約して /exchange に POST
```

利点:
- 複数アルゴの発注を 1 リクエストに集約 → rate limit 節約
- 並列発注衝突なしのため batch 内で nonce 整列
- HL 公式推奨パターンに完全準拠

### 7.3 Rate Limiter (Token Bucket)
```rust
pub struct TokenBucket {
    capacity: u32,        // 1200 (HL 通常) - margin (1000 程度に設定)
    tokens: AtomicU32,
    refill_rate: f64,     // 1000/60 = 16.67 tokens/sec
}
```

`PASSIVE_FOLLOW` / `MARKET_MAKE` の cancel/再発注ループが暴走時にも IP BAN を防ぐ.

---

## 8. Cancel 戦略 (Gemini v2 反映)

**重要**: HL の nonce 無効化は **in-flight (未到達) request を弾くのみ**, 板上の指値は cancel されない.
従って **cloid (client-generated 16-byte order id) + REST cancel API に統一**.

| 状況 | 方法 |
|---|---|
| 通常運用 (アルゴが自身の order を更新) | cloid で個別 cancel (REST batch endpoint) |
| アルゴ完了/abort 時 | アルゴが管理する全 cloid を一括 cancel (REST batch) |
| `/emergency_stop` | 全 open orders の cloid を REST batch cancel + 全アルゴ abort |
| Account-wide kill | 上記 + agent wallet の deregister (master 操作要、本 executor 範囲外) |

### 8.1 cloid 設計
- 16 bytes (= 32 hex chars). uuid v7 を採用 (時刻ソート可能 + 一意性)
- `cloid_to_string(uuid)` で API 用 hex に変換
- アルゴごとに発行. 1 アルゴが N 個の cloid を保持

### 8.2 nonce 無効化を使わない理由
- Gemini partner 指摘: 板上の指値は in-flight ではなく既に取引所側に登録済 → nonce 無効化では消えない
- 公式 doc は "rapid order placement" のための batch 推奨であり, cancel の代替ではない
- cloid + REST batch cancel が確実かつ docs 通りの方法

---

## 9. 公開 API (REST + WS)

### 9.1 REST
```
POST /execute
  body: {
    "exec_id": "uuid (optional)",
    "symbol": "xyz:SP500",
    "intent": "open" | "close" | "set_target",
    "target_size": 100.0,
    "algorithm": "twap" | "market" | "passive_follow" | "market_make",
    "params": { "duration_sec": 600, "slice_count": 12, "max_slippage_bps": 20 },
    "timeout_sec": 1800,
    "fallback_to_taker": false
  }
  response: { "exec_id": "...", "started_at": "..." }

POST /abort/{exec_id}
  → 進行中アルゴに abort signal. アルゴ管理 orders を個別 cancel

POST /emergency_stop
  → nonce 無効化で全 open orders 一掃 + 全アルゴ abort

GET /position/{symbol}
  → state engine から即答 (pos + open orders + 累計 filled)

GET /health
  → WS 接続生存, last book update, nonce window 残量
```

### 9.2 WebSocket
```
GET /ws/progress/{exec_id}
  ← server push:
    {"type": "started", "ts": "..."}
    {"type": "slice_filled", "slice": 3, "px": 100.5, "sz": 8.3, "cum": 25.0}
    {"type": "completed", "filled_size": 99.8, "avg_px": 100.7}
    {"type": "aborted", "reason": "..."}
    {"type": "error", "message": "..."}
```

---

## 10. Signer 抽象化 (Gemini v2: secrecy + zeroize)

```rust
use secrecy::{Secret, ExposeSecret};

#[async_trait]
pub trait Signer: Send + Sync {
    fn address(&self) -> Address;
    async fn sign_l1(&self, action: &Action, nonce: u64) -> Result<Signature>;
}

// 80% プロト用. 鍵不要で全体動作確認 (発注は in-memory 偽 response)
pub struct MockSigner { ... }

// 本番用. 環境変数 EXECUTOR_AGENT_PRIVATE_KEY から読み込み, secrecy で wrap
pub struct Eip712AgentSigner {
    inner: Secret<ethereum_private_key::PrivateKey>,
    address: Address,
}

impl Drop for Eip712AgentSigner {
    fn drop(&mut self) {
        // secrecy crate が Drop で zeroize 実行
    }
}
```

### 10.1 鍵漏洩リスク削減 (Gemini v2 反映)
- **`secrecy` crate** で `Secret<T>` wrap → ログ / Debug 出力に鍵が出ない
- **`zeroize` 連携** で Drop 時に memory 上の鍵をゼロクリア
- `String` / `Vec<u8>` での裸の保持を禁止 (clippy で検出可能)

### 10.2 Mock + Real の差し替え
80% プロト範囲では **MockSigner** で:
- WS 受信 / state 更新
- アルゴ実行 / 進捗送信
- API endpoint
- nonce 管理
- batch sender / rate limiter
- 統合テスト

mainnet 投入時に `Eip712AgentSigner` に差し替えるだけで実発注できる.

---

## 11. 鍵管理 (段階)

### Phase 3.0 (testnet, mock signer)
- 鍵不要. MockSigner で全体動作確認
- testnet endpoint 接続も任意 (公開なので key 不要なリードオンリー操作のみ)

### Phase 3.5 (testnet, real signer)
- testnet agent wallet 生成
- `.env` の `EXECUTOR_AGENT_PRIVATE_KEY` (git ignore 済)
- testnet 発注 → 約定確認

### Phase 3.9 (mainnet 小額)
- mainnet agent wallet (master account から approve)
- systemd `EnvironmentFile` (root only readable)
- 1 日 max notional 制限 (config) を internal hard cap

### Phase 4 (institutional)
- KMS / Vault. 設計範囲外.

---

## 12. テスト戦略

### 12.1 unit
`cargo test` で各 trait impl のロジック (mock HL client + mock signer)

### 12.2 integration
- testnet `wss://api.hyperliquid-testnet.xyz/ws` の実 endpoint へ接続テスト (鍵不要のリードオンリー)
- mock HL server (axum で fake `/exchange` を立てる) で発注 round-trip テスト

### 12.3 dry-run mode
`--dry-run` フラグで発注 step のみログ出力 (mainnet 投入前の最終確認)

### 12.4 chaos test
- WS 切断/復帰
- nonce 競合
- タイムアウト発生
- HTTP 5xx

### 12.5 CI
- testnet read-only integration は CI でも回せる (鍵不要)
- mainnet 発注 integration は手動 (秘密鍵 + 残高が必要)

---

## 13. レイテンシ目標

| 経路 | 目標 |
|---|---|
| Python POST → axum 受信 | < 1 ms (localhost) |
| 内部 state lookup → Order 構築 | < 0.5 ms |
| EIP-712 sign | < 0.1 ms |
| HTTP keep-alive POST `/exchange` | RTT 依存 (VPS で < 50 ms 期待) |
| **executor 内部のみ合計** | **< 2 ms** (HL の RTT 除く) |

VPS NY なら end-to-end < 100 ms 期待 (HL 公式 median 200 ms より速い可能性).

---

## 14. PR 計画 (Gemini v2 反映: 8 PR)

| PR | 内容 | 鍵必要? |
|---|---|---|
| PR-1 | workspace 初期化 + executor-core (types, cloid, errors, AppState struct, **rust_decimal**) + tests | No |
| PR-2 | executor-hl: WS subscriber + reconciliation + Signer trait + MockSigner + NonceManager + **TokenBucket** + **BatchSender** | No (mock) |
| PR-3 | executor-algo: Algorithm trait + MARKET (IoC, slippage cap, cloid) | No (mock) |
| PR-4 | executor-algo: PASSIVE_FOLLOW (cloid 個別 cancel/再発注) | No (mock) |
| PR-5 | executor-algo: TWAP (composition) | No (mock) |
| PR-6 | executor-algo: MARKET_MAKE (target-driven, cloid 個別 cancel) | No (mock) |
| PR-7 | executor-server (axum REST + WS) + 全アルゴ統合 + Rust CI workflow | No (mock) |
| PR-8 | emergency_stop (全 cloid 一括 cancel) + Python 連携 sample + dry-run integration | No (mock) |
| **別 PR** | **Eip712AgentSigner (secrecy + zeroize)** | **Yes (mainnet 投入前)** |

### 14.1 80% プロト範囲
PR-1 〜 PR-8 を MockSigner で完成. 鍵を一切触らずに以下が動く:
- workspace ビルド & cargo test
- WS 経由の state 構築
- 各アルゴの logic (発注は mock response で round-trip)
- REST + WS API end-to-end
- emergency_stop の挙動

mainnet 投入前に MockSigner → Eip712AgentSigner に差し替え + 鍵管理ブレストを行う.

### 14.2 各 PR の DoD
- `cargo fmt --check` / `cargo clippy --all-targets --all-features -- -D warnings` 通過
- `cargo test` 全 pass
- `scripts/check_ci_local.sh` 通過 (Python 既存テストも壊さない)
- Gemini partner レビュー通過
- `develop` 経由で merge

---

## 15. 残課題 / 後フェーズ

- **Phase 4**: VWAP / ICEBERG, KMS 統合, multi-account, HL ノード自前運用
- **Audit-C** (WS+REST 冗長化) は Rust 側で実装するのが自然
- **EIP-712 実署名** (Eip712AgentSigner) は本ブレスト範囲外. 80% プロトで枠だけ用意, 鍵管理ブレスト後に実装

---

## 16. CI / コード規約

- `cargo fmt --check` / `cargo clippy -- -D warnings` / `cargo test` を CI で実行
- Python と同じ check_ci_local.sh パターンを Rust にも導入
- `unsafe` ブロック禁止 (#![forbid(unsafe_code)] 各クレートで)
- `tracing` ベースの構造化ログ (Python の structlog と互換的)
- 設定は `figment` or `config-rs` で YAML/env 読み込み

---

## 17. 参照
- [Hyperliquid Optimizing latency](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/optimizing-latency)
- [Nonces and API wallets](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/nonces-and-api-wallets)
- [Exchange endpoint](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint)
- v3 design: [`2026-05-04-v3-design.md`](2026-05-04-v3-design.md)
- audit pipeline: [`docs/audit/`](../audit/)
