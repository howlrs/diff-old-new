# PR-D1: WS subscriber 本実装 + REST polling fallback 設計

**作成日**: 2026-05-05
**ブランチ**: `feat/pr-d1-ws-subscriber` (実装時)
**親 spec**: HANDOFF §5.4 deferred + ユーザー要望 ($100 ETH long を maker 約定で構築)
**前提**: develop@e77520d (Phase 3.5 完成)
**Gemini deep review**: review_log (10 論点 + 4 SHOULD-FIX)

## 1. 目的

executor-server を起動したら **WS subscriber が自動で動き出し, fills/orderUpdates/l2Book が
AppState に流れ込み続ける状態** を作る. これにより PASSIVE_FOLLOW など全 algo が
mainnet で正しく動く ($recent_fills$ 経由の partial fill 検知).

## 2. 確定論点 (Gemini deep)

| 項目 | 採用 |
|---|---|
| agent address 取得 | **Signer::address() trait 追加 (Q1 flip)** |
| l2Book subscribe symbols | **allow-list 流用 (Q2 flip)** |
| REST fallback | **userFills のみ, 10s 周期, 429 exp backoff (S2)** |
| stale_after | 30s + HL app-level ping 仕様確認 (S3) |
| reconnect backoff | exp 1s → 60s |
| reconcile | **再接続直後 + 5min 周期 (Q6 flip)** |
| Fill dedup | **(oid, trade_id) 複合キー (S1 critical)** |
| WS handle | **ServerState に保持, Drop で abort (Q9 flip)** |
| health endpoint | ws 状態露出 |

## 3. アーキテクチャ

### 3.1 Signer trait 拡張

```rust
// executor-hl/src/signer.rs
pub trait Signer: Send + Sync + std::fmt::Debug {
    fn sign(&self, ...) -> Result<Signature, SignerError>;
    /// PR-D1: agent wallet address. WS subscriber が userFills/orderUpdates を
    /// agent address で subscribe するため必要.
    fn address(&self) -> executor_core::types::Address;
}
```

`Eip712AgentSigner`: alloy の `LocalSigner::address()` から hex 化. `MockSigner`: 固定値.

### 3.2 新規モジュール: `executor-hl::ws_subscriber`

```rust
// executor-hl/src/ws_subscriber.rs
pub struct WsSubscriberConfig {
    pub url: String,
    pub agent_address: Address,
    pub symbols: Vec<Symbol>,
    pub stale_after: Duration,                    // 30s
    pub reconnect_backoff_min: Duration,          // 1s
    pub reconnect_backoff_max: Duration,          // 60s
    pub rest_poll_interval: Duration,             // 10s
    pub reconcile_interval: Duration,             // 5min
}

pub struct WsSubscriberHandle {
    pub join: JoinHandle<()>,
    pub shutdown: Arc<AtomicBool>,
    pub status: Arc<WsStatus>,                    // health endpoint で参照
}

pub struct WsStatus {
    pub connected: AtomicBool,
    pub last_message_at: RwLock<Option<DateTime<Utc>>>,
    pub message_count: AtomicU64,
    pub reconnect_count: AtomicU32,
}

pub fn spawn_ws_subscriber(
    cfg: WsSubscriberConfig,
    manager: Arc<WsStateManager>,
    rest_client: Arc<dyn HlClient>,
) -> WsSubscriberHandle;
```

### 3.3 内部 task の挙動

```
main loop:
  cfg.symbols が空ならログ警告. シンボル無しでも fills/orderUpdates だけは subscribe する.
  loop until shutdown:
    1. tokio_tungstenite::connect_async → on success:
       a. subscribe(userFills, agent), subscribe(orderUpdates, agent)
       b. for sym in symbols: subscribe(l2Book, sym)
       c. 再接続直後 reconcile (Gemini critical):
            fetch_open_orders(agent), fetch_account_state(master) → AppState 上書き
            ※ master address は cfg では渡さず, manager 経由で AppState 既存値を見るか CLI flag
       d. spawn read loop:
            while let Some(msg) = ws.next().await:
              decode WsFrame → expand → manager.apply()
              status.last_message_at = Now, message_count++
       e. spawn watchdog (30s tick): if last_message_at > stale → close + reconnect
       f. spawn rest fallback (10s tick): if !connected || stale: fetch_user_fills_by_time
            since=last_seen_ts → manager.apply() (Fill dedup は AppState 側で)
       g. spawn 5min reconcile tick: fetch_account_state + fetch_open_orders → 短 RwLock write で上書き
    2. on disconnect → backoff sleep (exp) → reconnect_count++ → retry
    3. on shutdown → close + return
```

### 3.4 Fill dedup (S1 critical)

`AppState::recent_fills: VecDeque<Fill>` に既に実装済. **追加: HashSet<(OrderId, u64)>** で
dedup 用 trade_id (HL の `tid` フィールド) を保持.

```rust
// executor-core/src/state.rs に追加
pub struct AppState {
    // ... 既存 ...
    pub seen_trade_ids: RwLock<HashSet<(OrderId, u64)>>,    // PR-D1
}
```

`WsStateManager::apply_fill` の先頭で:
```rust
let key = (OrderId(f.oid), f.tid);
{
    let mut seen = self.state.seen_trade_ids.write().await;
    if !seen.insert(key) {
        return;   // 既に処理済 fill (REST fallback と WS の重複)
    }
}
// 既存 fill push 処理...
```

### 3.5 wire types (`executor-hl::wire::ws`)

新規ファイル. HL WS の frame 構造を decode するための serde structs.

```rust
#[derive(Deserialize)]
#[serde(tag = "channel", rename_all = "camelCase")]
pub enum WsFrame {
    SubscriptionResponse(serde_json::Value),  // ack 捨てる
    Pong,                                      // app-level pong
    L2Book { data: WireL2BookData },
    UserFills { data: WireUserFillsData },
    OrderUpdates { data: Vec<WireOrderUpdate> },
}
```

decode 後、`WsFrame::UserFills(data)` → `Vec<WsMessage::UserFill>` に展開して
逐次 `manager.apply()` を呼ぶ.

### 3.6 main.rs での起動

```rust
// real mode のみ
if matches!(args.mode, Mode::Real) {
    let agent_addr = signer.address();   // Signer trait flip
    let symbols: Vec<Symbol> = match &safety.allow_symbols {
        Some(s) if !s.is_empty() => s.iter().cloned().collect(),
        _ => parse_csv(&args.ws_l2_symbols_fallback),  // '*' 時のみ
    };
    let url = match args.base {
        Base::Mainnet => "wss://api.hyperliquid.xyz/ws".into(),
        Base::Testnet => "wss://api.hyperliquid-testnet.xyz/ws".into(),
    };
    let manager = Arc::new(WsStateManager::new(state.app_state.clone()));
    let ws_cfg = WsSubscriberConfig { url, agent_address: agent_addr, symbols, ... };
    let ws_handle = spawn_ws_subscriber(ws_cfg, manager, state.hl_client.clone());
    state_mut.ws_handle = Some(ws_handle);     // ServerState に保持
}
```

### 3.7 ServerState への追加

```rust
pub struct ServerState {
    // 既存...
    pub ws_handle: Option<Arc<WsSubscriberHandle>>,
    pub ws_status: Arc<WsStatus>,    // health で参照
}

impl Drop for ServerState {
    fn drop(&mut self) {
        if let Some(h) = self.ws_handle.take() {
            h.shutdown.store(true, Ordering::Release);
            // join は async なので drop では待てない. PR-D4 で wire-up
        }
    }
}
```

### 3.8 Health endpoint 拡張

```rust
// executor-core/src/state.rs HealthStatus
pub struct HealthStatus {
    // 既存...
    pub ws_connected: bool,
    pub ws_last_message_at: Option<DateTime<Utc>>,
    pub ws_reconnect_count: u32,
}
```

`/v1/health` route で `ws_status` から値を吸い上げて応答.

## 4. CLI flag 追加

```
--ws-l2-symbols-fallback "ETH,BTC"   # allow-list が '*' のときのみ参照. default = "ETH,BTC"
```

注: 通常運用では allow-list の symbols がそのまま WS の l2Book subscribe target.
allow-list が `*` の場合のみ独立 fallback を使う.

## 5. テスト

### 5.1 wire/ws.rs decoder unit (~7 件)
- l2Book frame
- userFills (snapshot=true / false)
- orderUpdates (open / partiallyFilled / cancelled / filled)
- subscriptionResponse / pong
- 不正 JSON

### 5.2 fill dedup test
- 同じ (oid, tid) を 2 回 apply → 2 回目は no-op

### 5.3 ws_subscriber smoke
- 実 mainnet 接続 + subscribe + 1 message 受信を mainnet smoke (CI 外) で検証

## 6. mainnet smoke 計画 (PR merge 後)

ユーザー実行:
```bash
source scripts/load-env.sh
cd executor
cargo build --release -p executor-server
cargo run --release -p executor-server -- \
  --mode real --base mainnet \
  --mainnet-allow-symbols ETH \
  --mainnet-max-notional-usd 25 \
  --master-address 0xfe3e32cd...
```

期待 log:
```
INFO safety gate constructed allow=Some({ETH}) max=Some(25)
INFO MetaCache built (default dex) symbols=N
INFO BaselineGuard captured baseline_size=N
INFO ws_subscriber: connecting wss://api.hyperliquid.xyz/ws
INFO ws_subscriber: subscribed userFills+orderUpdates+l2Book(ETH)
INFO ws_subscriber: reconcile snapshot applied (open_orders=N, positions=N)
INFO executor-server listening 0.0.0.0:8085
```

別 terminal:
```bash
curl http://localhost:8085/v1/health | jq
# 期待: ws_connected=true, ws_last_message_at recent

curl -X POST http://localhost:8085/v1/exec \
  -H 'Content-Type: application/json' \
  -H 'X-Operator-ID: me@desk' \
  -d '{
    "algorithm":"passive",
    "symbol":"ETH",
    "intent":"open",
    "target_size":"0.005",
    "params":{"max_total_ms":120000,"repost_poll_ms":2000,"max_book_age_ms":5000}
  }'
# 期待: 200 + exec_id 返る. 数秒以内に best_bid に ALO post-only resting.
# market が下がってきたら maker fill. /v1/positions で ETH long 確認.
```

ユーザー指示 (2026-05-05):
- テスト発注は 0.005 ETH/注文
- long 約定後はポジ保持 (cancel 不要)
- 約定したら次のステップへ進む

## 7. 実装ファイル

```
executor/crates/executor-hl/src/wire/ws.rs           NEW: WsFrame + decoder
executor/crates/executor-hl/src/wire/mod.rs          MOD: pub mod ws
executor/crates/executor-hl/src/ws_subscriber.rs     NEW: spawn_ws_subscriber + tasks
executor/crates/executor-hl/src/lib.rs               MOD: pub mod ws_subscriber + re-exports
executor/crates/executor-hl/src/signer.rs            MOD: Signer::address() trait
executor/crates/executor-hl/src/hl_client.rs         MOD: fetch_user_fills_by_time API
executor/crates/executor-core/src/state.rs           MOD: HealthStatus に ws_*, AppState に seen_trade_ids
executor/crates/executor-server/src/state.rs         MOD: ServerState に ws_handle / ws_status
executor/crates/executor-server/src/main.rs          MOD: real mode で spawn_ws_subscriber
executor/crates/executor-server/src/routes.rs        MOD: health response に ws_* 反映
docs/HANDOFF-2026-05-05.md                            MOD: §13 PR-D1 完了 + Phase 4 開始宣言
```

## 8. 受入条件

- [ ] cargo build --workspace clean
- [ ] cargo test --workspace 全 pass (新 wire decoder + fill dedup + 既存無影響)
- [ ] cargo clippy / fmt clean / scripts/check_ci_local.sh green
- [ ] mainnet 起動で `ws_connected=true` `ws_last_message_at` 直近
- [ ] PASSIVE_FOLLOW 0.005 ETH で smoke → 約定 → /v1/positions に ETH long 反映 (ユーザー実機)
