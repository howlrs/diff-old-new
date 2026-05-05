# PR-C2: symbol allowlist + size cap (mainnet safety gate) 設計

**作成日**: 2026-05-05
**ブランチ**: `feat/pr-c2-mainnet-safety-gate` (実装時に作成)
**親 spec**: `docs/superpowers/specs/2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md` Stage C (production path 移行)
**前提コード**: PR-C1 merged (`develop@2a5dc44`)
**Gemini deep review**: 2026-05-05, `review_log:awei5shyz83tjs2lxpu3` (8 論点全クリア / Q3,Q4 で Claude 案を flip)

## 1. 目的

executor-server に **2 段の defense-in-depth gate** を入れ, mainnet 投入時の誤発注 (algo bug / 設定ミス) を
構造的に防ぐ. これにより executor-server を放置運用できる状態を作る.

具体的には:

1. **CLI flag** `--mainnet-allow-symbols ETH,BTC` `--mainnet-max-notional-usd 20` を追加
2. **Layer 1 (REST 入口)**: `POST /v1/exec` で symbol が allow-list 外 / 概算 notional が cap 超過 → 400 Bad Request
3. **Layer 2 (BatchSender enqueue 直前)**: algo が動的生成した `OrderIntent` を gate でチェック, 違反は drop + ack に Err
4. **mock mode は gate 全 skip** (CI 既存 145 tests 無影響)
5. **mainnet で allow-list 空 = fatal error 起動拒否** (Gemini 指摘: warn は見落とされる)
6. **`OrderOrCancel::Cancel` は gate を通さない** (緊急停止経路を阻害しないため)

これにより:
- algo がバグって ETH 以外 (HYPE / xyz:META / xyz:GOOGL etc.) に発注しようとしても server が拒否
- 「Master EOA 既存ポジを絶対に触らない」契約が CLI flag だけで保証可能
- production 投入時に `--mainnet-allow-symbols ETH --mainnet-max-notional-usd 20` で起動するだけで safe

## 2. 非目的

- baseline-diff guard 自動 emergency_stop (PR-C3)
- emergency_stop multi-symbol live test + e2e live test (PR-C4)
- 動的 (runtime) allow-list 変更 (Phase 4 以降)
- mock mode への gate 適用 (CI 既存テストへの影響回避)
- HL min notional / tick size の構造的検証 (HL 側 reject に委ねる; 別レイヤー)

## 3. 制約と前提

### 3.1 既存コード状況 (2026-05-05, PR-C1 merged 後)

| 項目 | 現状 |
|---|---|
| `executor-server` `Args` | `--mode mock\|real` `--base mainnet\|testnet` `--bind` の 3 flag |
| `BatchSender::enqueue(item: OrderOrCancel) -> Result<(), HlError>` | gate なし (素通し) |
| `OrderRouter::supported()` | `["market", "passive", "twap", "market_make"]` |
| `start_exec` handler | `validate_algorithm_name` の後すぐ `OrderRouter::build` → `tokio::spawn` |
| `ServerState` | `app_state, hl_client, signer, batch_sender, batch_handle, registry` |
| `OrderIntent` (PR-C1) | `cloid, symbol, side, px, sz, tif, reduce_only` (asset field は除去済) |
| `Symbol` | `executor-core::symbol::Symbol` (`String`-based newtype, `as_str()`) |

### 3.2 設計上の固定制約 (Gemini deep + ユーザー指示)

- **Q3 採用**: `--mode real --base mainnet` かつ `--mainnet-allow-symbols=""` は **fatal error** で起動拒否
- **Q4 採用**: `--mainnet-max-notional-usd` は `Option<u64>` (省略可能, 省略 = 上限なし)
- **Q1 採用**: gate 配置は **選択肢 C** (`BatchSender` に `Option<Arc<dyn IntentChecker>>` を持たせる)
- mock mode の gate disable は **`SafetyGate::disabled()` sentinel** で実装 (allow_symbols=None かつ max=None)
- `OrderOrCancel::Cancel` は gate チェック対象外
- `start_exec` の Layer 1 で notional 計算に best_bid を使う (rough fail-fast)
- testnet mode でも gate は適用 (gate 自体のテスト場として活用)
- error message に allowed symbol リストを含める (内部 API なので機密上問題なし)

## 4. アーキテクチャ

### 4.1 crate 配置とレイヤリング

```
executor-core    : OrderIntent / Symbol / ... (既存, 変更なし)
executor-hl      : BatchSender + IntentChecker trait (新規 trait, 最小 4 行)
                   + spawn_batch_sender_with_gate() 新設
executor-server  : SafetyGate 構造体 (実装) + impl IntentChecker for SafetyGate
                   + safety.rs (新規ファイル)
                   + main.rs に CLI flag + 起動時 gate 構築
                   + routes.rs::start_exec に Layer 1 gate
```

`IntentChecker` trait を `executor-hl` に置く理由: BatchSender が trait を呼ぶため.
`SafetyGate` の実装は `executor-server` に置く (CLI flag 由来であり Wire format とは無関係).
`OrderIntent` 自体は `executor-core` 由来なので, leaky にはならない.

### 4.2 SafetyGate 構造体

```rust
// executor-server/src/safety.rs
use std::collections::HashSet;
use rust_decimal::Decimal;
use executor_core::intent::OrderIntent;
use executor_core::symbol::Symbol;
use executor_hl::batch_sender::IntentChecker;

#[derive(Debug, Clone)]
pub struct SafetyGate {
    /// `None` = gate disabled (mock mode). `Some(set)` = active allow-list.
    pub allow_symbols: Option<HashSet<Symbol>>,
    /// `None` = no notional cap. `Some(usd)` = cap.
    pub max_notional_usd: Option<Decimal>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum SafetyViolation {
    #[error("symbol_not_allowed: symbol={symbol}, allowed={allowed:?}")]
    SymbolNotAllowed { symbol: Symbol, allowed: Vec<Symbol> },
    #[error("notional_exceeded: symbol={symbol}, notional={notional}, max={max}")]
    NotionalExceeded { symbol: Symbol, notional: Decimal, max: Decimal },
}

impl SafetyGate {
    pub fn disabled() -> Self;

    /// Build from CLI args. Returns Err if mainnet+real+empty allow-list.
    pub fn from_args(
        allow_csv: &str,
        max_usd: Option<u64>,
        is_mainnet_real: bool,
    ) -> Result<Self, anyhow::Error>;

    /// Layer 1 (REST entry): rough fail-fast using a reference price.
    /// `ref_px = None` skips the notional check (book unavailable).
    pub fn check_request(
        &self,
        symbol: &Symbol,
        target_size: Decimal,
        ref_px: Option<Decimal>,
    ) -> Result<(), SafetyViolation>;

    /// Layer 2 (enqueue): strict check using actual order px.
    pub fn check_intent(&self, intent: &OrderIntent) -> Result<(), SafetyViolation>;
}

impl IntentChecker for SafetyGate {
    fn check_place(&self, o: &OrderIntent) -> Result<(), String> {
        self.check_intent(o).map_err(|v| v.to_string())
    }
}
```

### 4.3 IntentChecker trait

```rust
// executor-hl/src/intent_checker.rs (新規ファイル, ~10 LOC)
use executor_core::intent::OrderIntent;

pub trait IntentChecker: std::fmt::Debug + Send + Sync + 'static {
    /// Return `Err(reason)` to reject the place. `Ok(())` to allow.
    fn check_place(&self, intent: &OrderIntent) -> Result<(), String>;
}
```

### 4.4 BatchSender 改修

```rust
// executor-hl/src/batch_sender.rs

#[derive(Clone)]
pub struct BatchSender {
    tx: mpsc::Sender<Envelope>,
    /// `None` = gate disabled. Set via spawn_batch_sender_with_gate.
    gate: Option<Arc<dyn IntentChecker>>,
}

impl BatchSender {
    pub fn enqueue(&self, item: OrderOrCancel) -> Result<(), HlError> {
        // Layer 2 gate
        if let (Some(gate), OrderOrCancel::Place(intent)) = (&self.gate, &item) {
            if let Err(reason) = gate.check_place(intent) {
                tracing::error!(reason, ?intent.cloid, ?intent.symbol, "safety_gate: drop place");
                return Err(HlError::ActionFormat(format!("safety_gate: {reason}")));
            }
        }
        self.tx.try_send(Envelope { item, ack: None })
            .map_err(|e| HlError::Network(format!("batch enqueue: {e}")))
    }

    // enqueue_with_ack も同じ gate を通す
}

pub fn spawn_batch_sender<C>(client: Arc<C>, cfg: BatchSenderConfig) -> (BatchSender, BatchSenderHandle)
where C: HlClient + 'static
{
    spawn_batch_sender_with_gate(client, None, cfg)
}

pub fn spawn_batch_sender_with_gate<C>(
    client: Arc<C>,
    gate: Option<Arc<dyn IntentChecker>>,
    cfg: BatchSenderConfig,
) -> (BatchSender, BatchSenderHandle)
where C: HlClient + 'static
{
    let (tx, rx) = mpsc::channel(1024);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let join = tokio::spawn(flusher_loop(client, rx, cfg, shutdown_rx));
    (
        BatchSender { tx, gate },
        BatchSenderHandle { join, shutdown: shutdown_tx },
    )
}
```

`flusher_loop` 内部の `flush_now` は変更不要 (gate は enqueue 時にすでに通過している).

### 4.5 main.rs CLI flag 追加

```rust
#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "mock")]
    mode: Mode,
    #[arg(long, default_value = "mainnet")]
    base: Base,
    #[arg(long, env = "EXECUTOR_BIND", default_value = "0.0.0.0:8085")]
    bind: String,

    /// Mainnet allow-list of symbols. Comma-separated (e.g. "ETH,BTC").
    /// REQUIRED for `--mode real --base mainnet`. Use `*` for explicit allow-all.
    #[arg(long, env = "EXECUTOR_MAINNET_ALLOW_SYMBOLS", default_value = "")]
    mainnet_allow_symbols: String,

    /// Mainnet hard cap on per-order notional (USD).
    /// Omit for no cap (NOT recommended for prod).
    #[arg(long, env = "EXECUTOR_MAINNET_MAX_NOTIONAL_USD")]
    mainnet_max_notional_usd: Option<u64>,
}
```

main 処理:
```rust
let is_mainnet_real = matches!(args.mode, Mode::Real) && matches!(args.base, Base::Mainnet);

let safety = match args.mode {
    Mode::Mock => Arc::new(SafetyGate::disabled()),
    Mode::Real => Arc::new(SafetyGate::from_args(
        &args.mainnet_allow_symbols,
        args.mainnet_max_notional_usd,
        is_mainnet_real,
    ).context("safety gate construction failed")?),
};

tracing::info!(
    allow_symbols = ?safety.allow_symbols,
    max_notional_usd = ?safety.max_notional_usd,
    "safety gate constructed",
);

let gate_dyn: Option<Arc<dyn IntentChecker>> = match args.mode {
    Mode::Mock => None,
    Mode::Real => Some(safety.clone() as Arc<dyn IntentChecker>),
};

let (batch_sender, batch_handle) = spawn_batch_sender_with_gate(
    /* hl_client */ ..., gate_dyn, batch_cfg,
);
```

`SafetyGate::from_args`:
```rust
pub fn from_args(
    allow_csv: &str,
    max_usd: Option<u64>,
    is_mainnet_real: bool,
) -> anyhow::Result<Self> {
    let allow_symbols = if allow_csv.trim().is_empty() {
        if is_mainnet_real {
            anyhow::bail!(
                "--mainnet-allow-symbols is required for --mode real --base mainnet. \
                 Use '*' to explicitly allow all (NOT recommended)."
            );
        }
        Some(HashSet::new())  // testnet+real で空指定: 何も通さない (= 厳しい側)
    } else if allow_csv.trim() == "*" {
        None  // 明示 allow-all (Symbol チェックを skip)
    } else {
        Some(allow_csv.split(',')
            .map(|s| Symbol::new(s.trim().to_string()))
            .collect())
    };
    let max_notional_usd = max_usd.map(Decimal::from);
    Ok(Self { allow_symbols, max_notional_usd })
}
```

注: `SafetyGate::disabled()` は `allow_symbols = None, max_notional_usd = None`.
`*` allow-all も同じ意味の `None`. distinction は不要 (両者 effect 同じ).
testnet+real で空指定は `Some(empty)` = 全 reject. 「testnet で gate 体感テスト」のため.

### 4.6 ServerState への追加

```rust
pub struct ServerState {
    pub app_state: Arc<AppState>,
    pub hl_client: Arc<dyn HlClient>,
    pub signer: Arc<dyn Signer>,
    pub batch_sender: BatchSender,
    pub batch_handle: tokio::sync::Mutex<Option<BatchSenderHandle>>,
    pub registry: ExecutionRegistry,
    pub safety: Arc<SafetyGate>,    // 新規
}

impl ServerState {
    pub fn new(
        app_state: Arc<AppState>,
        hl_client: Arc<dyn HlClient>,
        signer: Arc<dyn Signer>,
        batch_sender: BatchSender,
        batch_handle: BatchSenderHandle,
        safety: Arc<SafetyGate>,    // 新規
    ) -> Self { ... }
}
```

### 4.7 routes.rs::start_exec の Layer 1

```rust
pub async fn start_exec(
    State(s): State<Arc<ServerState>>,
    Json(req): Json<StartExecRequest>,
) -> Result<Json<StartExecResponse>, ServerError> {
    validate_algorithm_name(&req.algorithm)?;

    let symbol = Symbol::new(req.symbol.clone());
    let ref_px = {
        let book_g = s.app_state.book.read().await;
        book_g.get(&symbol).and_then(|b| b.best_bid())
    };
    s.safety
        .check_request(&symbol, req.target_size, ref_px)
        .map_err(|v| ServerError::BadRequest(format!("safety_gate: {v}")))?;

    let mut algo = OrderRouter::build(&req.algorithm)?;
    // ... 既存処理
}
```

`SafetyGate::check_request`:
```rust
pub fn check_request(&self, symbol: &Symbol, target_size: Decimal, ref_px: Option<Decimal>)
    -> Result<(), SafetyViolation>
{
    if let Some(allowed) = &self.allow_symbols {
        if !allowed.contains(symbol) {
            return Err(SafetyViolation::SymbolNotAllowed {
                symbol: symbol.clone(),
                allowed: allowed.iter().cloned().collect(),
            });
        }
    }
    if let (Some(max), Some(px)) = (self.max_notional_usd, ref_px) {
        let notional = px * target_size;
        if notional > max {
            return Err(SafetyViolation::NotionalExceeded {
                symbol: symbol.clone(),
                notional,
                max,
            });
        }
    }
    Ok(())
}
```

`SafetyGate::check_intent` (Layer 2):
```rust
pub fn check_intent(&self, o: &OrderIntent) -> Result<(), SafetyViolation> {
    if let Some(allowed) = &self.allow_symbols {
        if !allowed.contains(&o.symbol) {
            return Err(SafetyViolation::SymbolNotAllowed { ... });
        }
    }
    if let Some(max) = self.max_notional_usd {
        let notional = o.px * o.sz;
        if notional > max {
            return Err(SafetyViolation::NotionalExceeded { ... });
        }
    }
    Ok(())
}
```

## 5. テスト計画

### 5.1 SafetyGate unit tests (`safety.rs::tests`, ~8 件)

1. `disabled_gate_passes_everything`: `disabled()` で `check_request`, `check_intent` 両方 OK
2. `allow_list_rejects_non_member`: `{ETH,BTC}` で symbol=XRP → `SymbolNotAllowed`
3. `allow_list_accepts_member`: 同上で symbol=ETH → Ok
4. `notional_cap_rejects_over`: max=$10 で px=$2400 sz=0.005 (=$12) → `NotionalExceeded`
5. `notional_cap_accepts_under`: 上記で sz=0.004 (=$9.6) → Ok
6. `from_args_mainnet_empty_fatal`: `from_args("", None, true)` → Err
7. `from_args_mainnet_star_allow_all`: `from_args("*", None, true)` → Ok, allow_symbols=None
8. `from_args_csv_parses`: `from_args("ETH, BTC", Some(20), true)` → Ok, allow_symbols={ETH,BTC}, max=20

### 5.2 BatchSender Layer 2 tests (`batch_sender.rs::tests`, ~3 件)

1. `enqueue_with_gate_rejects_violation`: `IntentChecker` mock が `Err("test")` 返す → `enqueue` が `HlError::ActionFormat`
2. `enqueue_with_gate_passes_ok`: mock が `Ok(())` → enqueue 成功 → flush で実 client 呼び出し
3. `enqueue_cancel_skips_gate`: gate あっても `OrderOrCancel::Cancel` は通る

### 5.3 Integration tests (`integration_rest.rs`, ~3 件)

`build_state_with_seed()` をオーバーロード or `build_state_with_safety(SafetyGate)` 追加:

1. `start_exec_symbol_not_allowed_400`: `SafetyGate { allow={BTC} }` で `symbol=ETH` start → 400
2. `start_exec_notional_exceeded_400`: `SafetyGate { max=$10 }` + 仕込み book で `target_size=0.5` → 400
3. `start_exec_within_caps_200`: 同上で `target_size=0.0001` → 200

### 5.4 既存テスト無影響性

`build_state_with_seed()` は `Arc::new(SafetyGate::disabled())` を ServerState::new に渡す形に修正.
既存 10 tests は全 pass のまま.

## 6. 実装順序 (Plan で詳細化)

1. `executor-hl/src/intent_checker.rs` 新規 + `lib.rs` で `pub mod intent_checker;`
2. `BatchSender` に `gate` field 追加 + `spawn_batch_sender_with_gate` 新設 + 既存 `spawn_batch_sender` を wrapper 化
3. `BatchSender::enqueue` / `enqueue_with_ack` で gate チェック (Place のみ)
4. `executor-server/src/safety.rs` 新規 (`SafetyGate`, `SafetyViolation`, `IntentChecker` impl)
5. `ServerState::new` に `safety: Arc<SafetyGate>` 追加
6. `routes.rs::start_exec` に Layer 1 gate 注入
7. `main.rs` に CLI flag + `from_args` 起動シーケンス + `spawn_batch_sender_with_gate` 呼び替え
8. `integration_rest.rs::build_state_with_seed` を `SafetyGate::disabled()` 渡しに修正
9. SafetyGate unit tests 追加 (8 件)
10. BatchSender Layer 2 tests 追加 (3 件)
11. Integration tests 追加 (3 件)
12. `cargo fmt && cargo clippy -D warnings && cargo test --workspace` 全 pass 確認
13. `scripts/check_ci_local.sh` green
14. HANDOFF doc 追記 / commit / push / PR (--base develop)

## 7. リスクとフォールバック

| リスク | 影響 | 対策 |
|---|---|---|
| `Arc<dyn IntentChecker>` の vtable オーバーヘッド | enqueue ホットパスで微小 | mock mode は `gate=None` で if-let-else 即パス. real mode は ETH 1 symbol だけなので無視可能 |
| `BatchSender::clone` で `Arc<dyn ...>` の Arc::clone コスト | enqueue 1 回 / order | 同上 |
| trait object の Debug 制約 | `BatchSender: Debug` derive 失敗 | manual `Debug` 実装で `gate` field を `gate: bool` 表記 |
| testnet で `from_args("", ...)` を allow-all と勘違い | testnet で全 reject になる | doc string に明記 + 起動 log で `allow_symbols=Some({})` 表示 |
| `SafetyGate::check_intent` で `o.px * o.sz` が overflow | rust_decimal の限界 (96-bit mantissa) | HL の px/sz はこの limit 以内 (実機検証済 PR-B2b) |

## 8. 受入条件

- [ ] `cargo build --workspace` clean
- [ ] `cargo test --workspace` 全 pass (既存 145 + 新規 ~14 = ~159 件以上)
- [ ] `cargo fmt --all -- --check` clean
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` clean
- [ ] `scripts/check_ci_local.sh` green
- [ ] `--mode real --base mainnet --mainnet-allow-symbols ""` で **fatal error 起動拒否** を log で確認
- [ ] `--mode real --base mainnet --mainnet-allow-symbols ETH --mainnet-max-notional-usd 20` で `safety gate constructed allow_symbols=Some({ETH}) max=Some(20)` log
- [ ] curl `start_exec(symbol=ETH, target_size=0.005)` 実機で 200 + place 発火
- [ ] curl `start_exec(symbol=BTC, target_size=0.005)` 実機で 400 + log
- [ ] CI green / PR merge 待ち

## 9. 関連 PR / Issue

- 親 spec: `2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md` Stage C
- 前 PR: PR-C1 (`develop@2a5dc44`)
- 次 PR: PR-C3 (baseline-diff guard 自動 emergency_stop)
- 最後 PR: PR-C4 (multi-symbol live test + e2e)
