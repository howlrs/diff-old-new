# PR-C3: baseline-diff guard (master EOA position 監視 + auto emergency_stop) 設計

**作成日**: 2026-05-05
**ブランチ**: `feat/pr-c3-baseline-diff-guard` (実装時に作成)
**親 spec**: `docs/superpowers/specs/2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md` Stage C
**前提コード**: PR-C2 merged (`develop@b06f0ca`)
**Gemini deep review**: 2026-05-05, `review_log` に記録 (10 論点全クリア + 5 SHOULD-FIX 取り込み済)

## 1. 目的

executor-server 起動時に **master EOA の全 perp ポジションを baseline として snapshot** し,
60s 周期で再取得 + diff を比較. **既存ポジ szi が baseline から変動 → 自動 emergency_stop 発火**.

PR-C2 (gate = 入口防御) と組み合わせて 2 段安全:

- **PR-C2 (Gate)**: algo が「ETH 以外の symbol」「過大 notional」を発注しようとした瞬間に拒否
- **PR-C3 (Guard)**: 万が一 gate を擦り抜けて HYPE / xyz:META / xyz:GOOGL に変動が起きた瞬間に検知 + 全停止

これにより HANDOFF §4.2 の「既存ポジを絶対に触らない」契約を **algo bug や設定ミス耐性** として保証.

## 2. 非目的

- WS subscription による即時検知 (PR-D 系)
- 片肺リスク (multi-order partial failure) の検知 (PR-C5 想定, 別レイヤー)
- POST `/v1/exec` 受付停止以外の高度なシャットダウン処理 (signal handler は別 PR)

## 3. 制約と前提

### 3.1 既存コード状況 (2026-05-05, PR-C2 merged 後)

| 項目 | 現状 |
|---|---|
| `executor-server` `Args` | `--mode mock\|real` `--base mainnet\|testnet` `--bind` `--mainnet-allow-symbols` `--mainnet-max-notional-usd` |
| `BatchSender::enqueue` | gate 統合済 (PR-C2). Place は `IntentChecker` で検査, Cancel は素通し |
| `routes.rs::emergency_stop` | HTTP endpoint. cancel-then-abort 順. `X-Operator-ID` header で audit |
| `HlClient::fetch_account_state(address, dex: Option<&str>)` | 存在 (PR-A). `AccountStateSnapshot` 返す |
| `AccountStateSnapshot.positions: HashMap<Symbol, Position>` | exists. `Position::size: Decimal` (= szi) |
| `Address` (executor-core) | exists. `Address::new(&str)` |
| Master EOA | `.env.develop::HL_MASTER_ADDRESS` (= `0xdbefbece...`). HANDOFF §4.2 の `0xfe3e32cd...` は別アドレスで既存ポジ持ち |

### 3.2 Gemini deep flip / 採用論点

- Q1: **`(Option<String>, Symbol)` tuple key** で baseline を持つ (String 連結はバグ温床)
- Q2: 5 連続 fetch 失敗で alarm. 単発失敗は `tracing::warn` のみ
- Q3: fire 後は server alive + `start_exec` を 503 拒否. **`AtomicBool` で冪等化**
- Q4: 起動時 pos 空でもゼロを baseline とする (paranoid: ゼロからの動きも検知)
- Q5: `--baseline-dexes` 明示指定 (`default,xyz`)
- Q6: `--baseline-szi-epsilon` configurable, default `0`
- Q7: 60s polling (WS は別 PR)
- Q8: tick task は fire 後に `break`
- Q9: mock mode は guard 無効
- Q10: 片肺リスクは別 PR
- **S1**: `RwLock` 不要. baseline は read-only `HashMap<...>`
- **S2**: 連続失敗カウンタを tick task ローカル変数で
- **S3**: emergency_stop の冪等性. `state.shutdown_initiated: AtomicBool`
- **S4**: `tokio::select!` で `ticker.tick()` と `shutdown_rx` を併記 (graceful 用)
- **S5**: 通知メカニズム統一. 直接呼び出しに揃え `watch::Sender<bool>` は撤回

## 4. アーキテクチャ

### 4.1 BaselineGuard 構造体

```rust
// executor-server/src/baseline.rs
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use rust_decimal::Decimal;
use executor_core::symbol::Symbol;
use executor_core::types::Address;
use executor_hl::hl_client::HlClient;

/// (dex, symbol) → szi at startup. Read-only after `capture()`.
pub type BaselineKey = (Option<String>, Symbol);

#[derive(Debug)]
pub struct BaselineGuard {
    pub baseline: HashMap<BaselineKey, Decimal>,
    pub master: Address,
    pub dexes: Vec<Option<String>>,
    pub poll_interval: Duration,
    pub szi_epsilon: Decimal,
}

#[derive(Debug, Clone)]
pub struct BaselineViolation {
    pub dex: Option<String>,
    pub symbol: Symbol,
    pub baseline_szi: Decimal,
    pub current_szi: Decimal,
    pub diff: Decimal,
}

impl BaselineGuard {
    /// Take a startup snapshot across all configured dexes.
    pub async fn capture<C>(
        client: &C,
        master: Address,
        dexes: Vec<Option<String>>,
        poll_interval: Duration,
        szi_epsilon: Decimal,
    ) -> anyhow::Result<Self>
    where
        C: HlClient + ?Sized;

    /// One periodic check. Returns violations (empty = clean).
    /// Returns `Err(_)` only when the fetch itself fails (caller decides
    /// whether to count this towards consecutive-error threshold).
    pub async fn check_once<C>(
        &self,
        client: &C,
    ) -> Result<Vec<BaselineViolation>, executor_hl::HlError>
    where
        C: HlClient + ?Sized;
}
```

### 4.2 fire_emergency_stop の冪等化

`routes.rs` の `emergency_stop` HTTP handler 内のロジックを共通関数に抽出:

```rust
// routes.rs

/// Idempotent kill switch. Returns the operation result whether or not we
/// were the first caller to flip the shutdown flag — first caller does the
/// real work, subsequent calls return the cached "already-done" snapshot.
pub async fn execute_emergency_stop(
    s: &Arc<ServerState>,
    operator: &str,
) -> EmergencyStopResponse {
    use std::sync::atomic::Ordering;

    // First-only gate: if we lose the race, return a no-op response.
    if s.shutdown_initiated
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        tracing::info!(operator, "emergency_stop: already initiated, skipping");
        return EmergencyStopResponse {
            aborted_executions: 0,
            cancelled_orders: 0,
        };
    }

    // 既存ロジック (open_orders snapshot → cancel batch enqueue → abort_all)
    // ...

    EmergencyStopResponse { aborted_executions, cancelled_orders }
}

pub async fn emergency_stop(
    State(s): State<Arc<ServerState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<EmergencyStopResponse>, ServerError> {
    let operator = headers.get("x-operator-id").and_then(|v| v.to_str().ok()).unwrap_or("unknown");
    Ok(Json(execute_emergency_stop(&s, operator).await))
}
```

### 4.3 ServerState への追加

```rust
pub struct ServerState {
    // ... existing fields ...
    pub safety: Arc<SafetyGate>,
    /// PR-C3: AtomicBool that becomes `true` once any caller (HTTP handler or
    /// BaselineGuard tick) initiates emergency_stop. Used to:
    /// - idempotency-gate `execute_emergency_stop`
    /// - reject new `start_exec` calls with 503 (PR-C3 behavior)
    pub shutdown_initiated: std::sync::atomic::AtomicBool,
}
```

`start_exec` 内に新規受付停止 check を追加:

```rust
// routes.rs::start_exec
if s.shutdown_initiated.load(Ordering::Acquire) {
    return Err(ServerError::ServiceUnavailable(
        "executor is in emergency_stop state; restart required".into()
    ));
}
```

`ServerError::ServiceUnavailable` (新 variant) → HTTP 503 にマップ.

### 4.4 起動シーケンス (main.rs)

```rust
// PR-C3: only in real mode (mock/CI is skipped)
let baseline_guard: Option<Arc<BaselineGuard>> = match args.mode {
    Mode::Mock => None,
    Mode::Real => {
        if !args.baseline_guard {
            tracing::warn!("baseline-guard disabled by CLI (NOT recommended)");
            None
        } else {
            let master = args.master_address
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("--master-address (or HL_MASTER_ADDRESS env) required for real mode + baseline_guard"))?;
            let dexes = parse_dexes_csv(&args.baseline_dexes);
            let g = BaselineGuard::capture(
                &*real_client,
                Address::new(master),
                dexes,
                Duration::from_secs(args.baseline_poll_secs),
                args.baseline_szi_epsilon.parse().unwrap_or(Decimal::ZERO),
            ).await.context("BaselineGuard::capture failed")?;
            tracing::info!(
                master = %master,
                dexes = ?g.dexes,
                baseline_size = g.baseline.len(),
                poll_secs = ?g.poll_interval.as_secs(),
                "BaselineGuard captured",
            );
            Some(Arc::new(g))
        }
    }
};
```

`shutdown_initiated` は `AtomicBool::new(false)` で server state に初期化.
guard tick task は Server state を持って spawn:

```rust
let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
if let Some(g) = baseline_guard.clone() {
    let client = real_client.clone();
    let st = state.clone();
    let max_consec_errors = args.baseline_max_consec_errors;
    let mut shutdown_rx = shutdown_rx.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(g.poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut consec_errors: u32 = 0;
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    match g.check_once(&*client).await {
                        Ok(violations) if violations.is_empty() => {
                            consec_errors = 0;
                            tracing::trace!("baseline_guard: tick clean");
                        }
                        Ok(violations) => {
                            tracing::error!(?violations, "BASELINE VIOLATION DETECTED");
                            execute_emergency_stop(&st, "baseline_guard").await;
                            tracing::error!("baseline_guard: emergency_stop fired, exiting tick loop");
                            break;
                        }
                        Err(e) => {
                            consec_errors += 1;
                            tracing::warn!(?e, consec_errors, "baseline_guard: fetch failed");
                            if consec_errors >= max_consec_errors {
                                tracing::error!(consec_errors, "baseline_guard: too many consecutive failures, firing emergency_stop");
                                execute_emergency_stop(&st, "baseline_guard_consec_errors").await;
                                break;
                            }
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::info!("baseline_guard: shutdown signal received, exiting");
                        break;
                    }
                }
            }
        }
    });
}
```

### 4.5 CLI flag 追加

```rust
/// Enable baseline-diff guard (real mode only).
#[arg(long, env = "EXECUTOR_BASELINE_GUARD", default_value_t = true)]
baseline_guard: bool,

/// Master EOA address to monitor (defaults to env HL_MASTER_ADDRESS).
#[arg(long, env = "HL_MASTER_ADDRESS")]
master_address: Option<String>,

/// Baseline snapshot polling interval (seconds).
#[arg(long, env = "EXECUTOR_BASELINE_POLL_SECS", default_value_t = 60)]
baseline_poll_secs: u64,

/// Comma-separated dex list to monitor (e.g. "default,xyz").
/// Use empty string for default-dex only.
#[arg(long, env = "EXECUTOR_BASELINE_DEXES", default_value = "default,xyz")]
baseline_dexes: String,

/// szi diff epsilon for baseline check ("0" for strict).
#[arg(long, env = "EXECUTOR_BASELINE_SZI_EPSILON", default_value = "0")]
baseline_szi_epsilon: String,

/// Maximum consecutive fetch failures before firing emergency_stop.
#[arg(long, env = "EXECUTOR_BASELINE_MAX_CONSEC_ERRORS", default_value_t = 5)]
baseline_max_consec_errors: u32,
```

`parse_dexes_csv` ヘルパ: `"default,xyz"` → `[None, Some("xyz")]`. `default` は `None` として扱う.

### 4.6 BaselineGuard::check_once 詳細

```rust
pub async fn check_once<C>(&self, client: &C)
    -> Result<Vec<BaselineViolation>, executor_hl::HlError>
where C: HlClient + ?Sized,
{
    let mut violations = Vec::new();
    let mut current_keys = std::collections::HashSet::new();
    for dex in &self.dexes {
        let snap = client.fetch_account_state(&self.master, dex.as_deref()).await?;
        for (sym, pos) in &snap.positions {
            let key: BaselineKey = (dex.clone(), sym.clone());
            current_keys.insert(key.clone());
            let baseline_szi = self.baseline.get(&key).copied().unwrap_or(Decimal::ZERO);
            let diff = (pos.size - baseline_szi).abs();
            if diff > self.szi_epsilon {
                violations.push(BaselineViolation {
                    dex: dex.clone(),
                    symbol: sym.clone(),
                    baseline_szi,
                    current_szi: pos.size,
                    diff,
                });
            }
        }
    }
    // Detect missing keys (= positions disappeared)
    for (key, baseline_szi) in &self.baseline {
        if !current_keys.contains(key) && *baseline_szi != Decimal::ZERO {
            violations.push(BaselineViolation {
                dex: key.0.clone(),
                symbol: key.1.clone(),
                baseline_szi: *baseline_szi,
                current_szi: Decimal::ZERO,
                diff: baseline_szi.abs(),
            });
        }
    }
    Ok(violations)
}
```

注意: 1 つの dex の fetch が失敗したら `?` で early return. `consec_errors` は呼び出し側 tick task で管理. 「全 dex の fetch が成功した時のみ violation 判定」が方針 (一部失敗時は監視継続不可と判断).

### 4.7 ServerError::ServiceUnavailable

```rust
// error.rs
#[derive(Debug, Error)]
pub enum ServerError {
    #[error("execution {0} not found")]
    NotFound(String),
    #[error("invalid request: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("internal error: {0}")]
    Internal(String),
    /// PR-C3: server is in emergency_stop state.
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let (status, code, msg) = match &self {
            // ... existing ...
            ServerError::ServiceUnavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                self.to_string(),
            ),
        };
        // ...
    }
}
```

## 5. テスト計画

### 5.1 BaselineGuard unit tests (`baseline.rs::tests`, ~6 件)

`MockHlClient::seed_account` を使う:

1. `capture_succeeds_with_seeded_positions`: `seed_account` で 2 dex 仕込み → `BaselineGuard::capture` で baseline = {(None,HYPE), (Some("xyz"),META)} の 2 entry
2. `check_once_returns_empty_when_unchanged`: 仕込みポジション維持 → `check_once = Ok([])`
3. `check_once_detects_size_increase`: capture 後 `mock.account.lock().await` で szi を変更 → violation 1 件
4. `check_once_detects_position_disappearance`: capture 時にあった HYPE が後で消えた → violation 1 件 (baseline_szi != 0, current_szi = 0)
5. `check_once_with_szi_epsilon_tolerates_small_drift`: `epsilon = 0.01` で diff = 0.005 → `Ok([])`
6. `check_once_propagates_fetch_error`: `MockHlClient::set_fail(true)` → `Err(HlError)` (要 mock 拡張)

注: 6 は `MockHlClient` に `set_fail(bool)` 機能が無いと実装できない. もし無ければ `mockito` ベースで `RealHlClient` と組み合わせた integration test に切り替え, または mock 拡張で対応.

### 5.2 fire_emergency_stop idempotency tests (`integration_rest.rs`, ~2 件)

1. `emergency_stop_idempotent`: 2 回連続 POST → 1 回目は `aborted=N cancelled=M`, 2 回目は `0,0`
2. `start_exec_after_emergency_stop_503`: emergency_stop 発火後 → `start_exec` が 503

### 5.3 整合性 (Layer 1/2 gate との独立性)

- guard 機能は `SafetyGate` と別の Concern. テストは独立.

### 5.4 既存 145+19 = 164 tests 影響なし

- mock mode で起動するテストは guard 起動経路を踏まないので影響なし.
- `ServerState::new` は既存 6 引数のまま (シャットダウンフラグは `Default::default()` で内部初期化).

## 6. 実装順序

1. `executor-server/src/baseline.rs` 新規 (BaselineGuard 構造体 + check_once)
2. `executor-server/src/lib.rs` で `pub mod baseline; pub use baseline::{BaselineGuard,BaselineViolation};`
3. `executor-server/src/state.rs` に `shutdown_initiated: AtomicBool` 追加 (`Default::default` 経由初期化)
4. `executor-server/src/error.rs` に `ServerError::ServiceUnavailable(String)` 追加 + 503 mapping
5. `executor-server/src/routes.rs` で `execute_emergency_stop` を抽出, 冪等化, `emergency_stop` HTTP handler を refactor; `start_exec` で 503 check
6. `executor-server/src/main.rs` に CLI flag 6 追加 + `BaselineGuard::capture` + tick task spawn
7. BaselineGuard unit tests 追加 (6 件)
8. integration_rest tests 追加 (2 件)
9. fmt / clippy / test --workspace / scripts/check_ci_local.sh
10. HANDOFF doc 追記
11. commit / push / PR (--base develop)

## 7. リスクとフォールバック

| リスク | 影響 | 対策 |
|---|---|---|
| `MockHlClient::set_fail` 拡張がコスト高 | テスト 6 がブロック | mockito ベース (RealHlClient) で代替. 既存 `place_cancel_mock.rs` パターン流用 |
| start_exec 503 の挙動が algo runtime に影響 | algo は何も知らないが新規 start_exec が拒否されるだけ | OK. 既に algorithm 起動済のものは abort_all でちゃんと止まる |
| 60s tick の rate limit 消費 | HL info `clearinghouseState` を 2 dex × 1/min = 2 req/min | 余裕. HL の info rate limit は 数 req/sec 級 |
| AtomicBool 経路の Rust の `compare_exchange` ordering 選択ミス | 同時発火時にロジック乱れ | `AcqRel` (success) / `Acquire` (failure) で memory_order 厳密に. Loom test までは不要 |
| guard が fire 直前に signal 受信 → break しない | shutdown 経路と矛盾 | `tokio::select!` で `shutdown_rx.changed()` も同時に待つ. fire が先勝ちなら break, signal が先勝ちなら break. どちらでも terminate |
| 起動時 master EOA の HIP-3 dex で fetch が失敗 | `BaselineGuard::capture` が `?` で early return → server 起動失敗 | 設計通りの想定挙動. 起動時失敗ならば user に明示 |
| 連続失敗閾値 5 が短すぎ | network hiccup で false fire | CLI flag で可変. default 5 = 5 min 連続失敗 |

## 8. 受入条件

- [ ] `cargo build --workspace` clean
- [ ] `cargo test --workspace` 全 pass (164 + 新規 ~8 = ~172)
- [ ] `cargo fmt --all -- --check` clean
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean
- [ ] `scripts/check_ci_local.sh` green
- [ ] `cargo run -p executor-server -- --mode real --base mainnet --mainnet-allow-symbols ETH --mainnet-max-notional-usd 20` で `BaselineGuard captured master=... dexes=[None, Some("xyz")] baseline_size=N` log
- [ ] (ユーザー) 既存 master EOA で起動して baseline log 出ることを確認
- [ ] CI green / merge 後 develop へ

## 9. 関連 PR / Issue

- 親 spec: `2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md` Stage C
- 前 PR: PR-C2 (`develop@b06f0ca`)
- 次 PR: PR-C4 (multi-symbol live test + e2e)
