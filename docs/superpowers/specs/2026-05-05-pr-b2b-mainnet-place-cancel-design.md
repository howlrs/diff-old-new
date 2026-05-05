# PR-B2b: mainnet 極小発注 + cancel 1 往復検証 設計

**作成日**: 2026-05-05
**ブランチ**: `feat/pr-b2b-mainnet-place-cancel` (実装時に作成)
**親 spec**: `docs/superpowers/specs/2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md` Stage C §7
**前提コード**: PR-B2a merged (`develop@e3ee210`)

## 1. 目的

`Eip712AgentSigner` + `RealHlClient::place_orders` / `cancel_orders` を **mainnet HL に対して**
1 度だけ実発注 → 即時キャンセルし, **wire format / signature acceptance / response shape** が
実環境で動作することを確認する.

ユーザー指示: 「mainnet で各情報を取得できるまでになっていれば mainnet 注文フェーズに移行,
不必要な繰り返し loop なし前提でミニサイズなら問題ない」.

ユーザー指示: 「銘柄のハードコート (テストコード内なら OK)」.

## 2. 非目的

- testnet 経由検証 (skip 確定)
- `fetch_meta` キャッシュの本実装組み込み (テストローカルで取得のみ)
- vault / subaccount サポート
- WS subscriber 本実装
- 本番運用 (executor-server 経由の継続発注)
- emergency_stop の本格検証 (本 PR は単発キャンセル)
- mainnet で複数銘柄 / 複数往復 / 連続発注

## 3. 制約と前提

### 3.1 既存コード状況 (2026-05-05, PR-B2a merged 後)

| 機能 | 状態 |
|---|---|
| `Eip712AgentSigner::from_secret(SecretString, is_mainnet)` | ✅ HL python-sdk と byte-identical (10/10) |
| `RealHlClient::place_orders` | ✅ 実装済 (mock 8/8 pass), wire 経由は未検証 |
| `RealHlClient::cancel_orders` | ✅ 実装済 (`cancelByCloid`), wire 経由は未検証 |
| `RealHlClient::fetch_account_state` / `fetch_open_orders` / `fetch_meta` | ✅ live mainnet で実証済 (PR-A) |
| pass-store の agent PK | ✅ 保管済 (`~/.password-store/diff-old-new/hl/agent-pk.gpg`) |
| Claude PreToolUse hook で PK アクセス block | ✅ active (`.claude/hooks/deny-pk-*.sh`) |

### 3.2 mainnet 既存ポジション (read-only snapshot 2026-05-05 取得)

Master EOA `0xfe3e...7d2d`:

| dex | 種別 | symbol | 注意 |
|---|---|---|---|
| default | perp position | **HYPE** | long, cross. 触らない |
| xyz (HIP-3) | perp position | **xyz:META** | long, cross. 触らない |
| xyz | perp open order | **xyz:GOOGL** | bid 1 件. 触らない |
| spot | balance | USDC | $2,477 (内 $1,162 hold). 触らない |

### 3.3 マージン余力 (default dex)

- `accountValue=$643.72`, `withdrawable=$34.87`
- ETH $2 notional × 10x leverage = $0.20 証拠金で十分
- 1 注文だけなので nonce 衝突なし

### 3.4 r/s padding 問題 (PR-B1 で識別)

HL python-sdk は `eth_utils.to_hex(int)` で leading zero strip (例 63 hex). Rust は常に 64 hex padded.
**本 PR で実 HL が padded を accept するか検証**.
- accept されれば: 既存実装そのままで良い
- reject されれば: signer.rs に `format!("0x{:x}", ...)` 切り替え (leading zero strip) が必要 → 本 PR で fix

## 4. 設計

### 4.1 シナリオ

mainnet `0xfe3e...7d2d` (master EOA) + agent wallet `0xB2a7...b8c5` を使い:

1. **pre-snapshot**: `fetch_account_state` (default dex + xyz dex) + `fetch_open_orders` を取得し JSON 保存
2. **ETH index 解決**: `fetch_meta(None)` で universe を取得し ETH の `asset_index` を確認 (期待値 = 1)
3. **best price 取得**: `fetch_book_snapshot(Symbol::new("ETH"))` で best_bid を取得
4. **order 構築**: `OrderIntent { cloid: <new>, symbol: "ETH", asset: <eth_idx>, side: Long, px: best_bid * 0.99, sz: 0.001, tif: Alo, reduce_only: false }`
5. **place**: `client.place_orders(&[intent]).await` → response の `oid` 取得 (resting status 期待)
6. **post-place wait**: 200ms wait (HL backend が ack するまでの marginal time)
7. **cancel**: `CancelIntent { symbol, asset, by_cloid: Some(cloid), by_oid: None }` → `client.cancel_orders(&[cancel]).await`
8. **post-snapshot**: pre と同じ endpoint を再取得
9. **diff 検証**:
   - HYPE position の `szi` 完全一致
   - xyz:META position の `szi` 完全一致
   - xyz:GOOGL open order の oid 同一
   - ETH 関連の open order が 0 件 (キャンセル成功)
   - master EOA の `accountValue` の差は $0.01 未満 (手数料余地のみ; ALO post-only reject なら 0)
10. **userFills check (オプション)**: `fetch_user_fills` (PR-B2b で必要なら追加, 現状未実装) で今回の cloid が `canceled` 状態
   - PR-B2b で `userFills` 用 wire 型追加までは scope 外、log で確認のみ
11. **完了報告**: 全 assertion pass ならログに「✓ mainnet round trip success」

### 4.2 4 重安全装置 (親 spec §7.2 から)

| 装置 | 実装 |
|---|---|
| **シンボル allowlist** | テストコード内で hardcode `Symbol::new("ETH")` のみ使用. 他 symbol の混入は cargo test で起きえない |
| **size 上限** | `dec!(0.001)` (~$2 at $2000/ETH). 上限定数 `MAX_NOTIONAL_USD = 5` で size * px のチェック |
| **baseline-diff guard** | pre/post snapshot の `assert_eq!` で HYPE/META/GOOGL position szi / oid の同一性を物理的に保証 |
| **best-far ALO post-only** | `px = best_bid * 0.99` (1% 下) + `tif: Alo`. HL は ALO クロス時に reject するため fill 0 を物理保証 |

### 4.3 テストの位置づけ

**`#[cfg(feature = "live")]`** + 既存の `executor-hl --features live` フラグを再利用.
ファイル: `executor/crates/executor-hl/tests/live_mainnet_place_cancel.rs` (新規).

CI では default off → 安全. ローカルで明示的に enable してユーザー本人が実行する:

```bash
# ユーザー側 (PK が要るので別 terminal で env load):
source scripts/load-env.sh
cd executor
HL_TEST_ADDRESS=0xfe3e32cd4443e395ec0400bf828a34309e517d2d \
HL_AGENT_PK=$HL_AGENT_PK \
cargo test -p executor-hl --features live live_mainnet_place_cancel \
  -- --nocapture --test-threads=1
```

`HL_AGENT_PK` は `scripts/load-env.sh` で `pass show` 経由 export 済.
`--test-threads=1` は他 live test との競合防止 (HL rate limit + nonce 衝突回避).

### 4.4 PK 取り扱い

- `Eip712AgentSigner::from_secret(SecretString::new(pk), true /* is_mainnet */)` で構築
- PK は env 経由のみ. test コード中に hardcode 禁止.
- Claude session で test 実行は不可能 (project hook が `pass show` を block, env 注入も block).
  → **ユーザー本人が別 terminal で実行**, 結果 ログを Claude に貼り付ける.
- 結果ログに r/s/v が混入する可能性: テスト output で `eprintln!` を制限し, signature dump はしない.

### 4.5 r/s padding 検証ロジック

テスト内で `place_orders` 結果を assert する際:
- response が `{resting: {oid: N}}` で受理されれば `r/s padding 64-hex でも HL は accept する` ことが証明される
- response が `{error: "<msg>"}` で msg に `signature` を含めば padding が問題. 即座にテストを fail させ, ログに `padding rejected, see signer.rs:r/s formatting` を出す.
  → 別 commit で signer.rs の `format!("0x{:064x}", ...)` を `format!("0x{:x}", ...)` に変更し再走

### 4.6 ロールバック手順

万一 fill した場合 (best_bid * 0.99 を超えて市場が急落):

1. テスト内で `place_orders` の response が `{filled: ...}` だった場合, 即時 `Result<(), Err("FILLED — manual recovery required")>` で abort
2. ユーザーが手動 (HL UI) で reduce-only 反対売買
3. 当該 commit を revert

cancel が失敗した場合:
1. テスト内で `cancel_orders` の response が `error` だった場合, 即 abort + log
2. ユーザーが手動 cancel (HL UI)
3. 原因調査

## 5. ファイル構成

| パス | 役割 | アクション |
|---|---|---|
| `executor/crates/executor-hl/tests/live_mainnet_place_cancel.rs` | mainnet 1 往復 e2e テスト. `#[cfg(feature = "live")]` | **新規** |
| `executor/crates/executor-hl/tests/live_mainnet_readonly.rs` | 既存 (PR-A). 触らない | (なし) |
| `executor/crates/executor-hl/Cargo.toml` | 既存の `[features] live` を再利用 | (なし) |
| `docs/HANDOFF-2026-05-04.md` | PR-B2b 完了行追加 | 修正 |
| `scripts/load-env.sh` | 既存 (PR-A infra). 触らない | (なし) |

新規 dependency なし. 全部既存資産で完結.

## 6. テストコード骨格

```rust
//! Live mainnet 1-round-trip place + cancel test.
//!
//! Hits real HL mainnet /exchange. Requires:
//! - HL_TEST_ADDRESS env: master EOA (read-only baseline)
//! - HL_AGENT_PK env: agent wallet 64-hex private key (from pass-store)
//!
//! Run only by the user from a shell where `source scripts/load-env.sh`
//! has been executed. Do NOT enable --features live in CI.

#![cfg(feature = "live")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_core::cloid::Cloid;
use executor_core::intent::{CancelIntent, OrderIntent};
use executor_core::symbol::Symbol;
use executor_core::types::{Address, Side, Tif};
use executor_hl::hl_client::{HlClient, HlConfig, RealHlClient};
use executor_hl::signer::Eip712AgentSigner;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use secrecy::SecretString;
use std::sync::Arc;
use std::time::Duration;

const MAX_NOTIONAL_USD: Decimal = dec!(5);
const ORDER_SZ_ETH: Decimal = dec!(0.001);
const PRICE_OFFSET_RATIO: Decimal = dec!(0.99);

fn master_address() -> Address {
    let s = std::env::var("HL_TEST_ADDRESS")
        .expect("HL_TEST_ADDRESS env required (master EOA)");
    Address::new(s)
}

fn agent_pk_secret() -> SecretString {
    let s = std::env::var("HL_AGENT_PK")
        .expect("HL_AGENT_PK env required (source scripts/load-env.sh first)");
    SecretString::new(s.into())
}

fn make_client() -> RealHlClient {
    let signer = Arc::new(
        Eip712AgentSigner::from_secret(agent_pk_secret(), true /* is_mainnet */).unwrap()
    );
    RealHlClient::new(HlConfig::mainnet(), signer)
}

#[tokio::test]
async fn live_mainnet_place_cancel_eth_round_trip() {
    let client = make_client();
    let master = master_address();

    // === pre-snapshot ===
    let pre_default = client.fetch_account_state(&master, None).await
        .expect("fetch default state pre");
    let pre_xyz = client.fetch_account_state(&master, Some("xyz")).await
        .expect("fetch xyz state pre");
    let pre_xyz_orders = client.fetch_open_orders(&master, Some("xyz")).await
        .expect("fetch xyz orders pre");

    eprintln!("PRE: default positions={}, xyz positions={}, xyz orders={}",
              pre_default.positions.len(), pre_xyz.positions.len(), pre_xyz_orders.len());

    // === ETH index resolve ===
    let meta = client.fetch_meta(None).await.expect("fetch meta");
    let eth_idx = meta.universe.iter().position(|u| u.name == "ETH")
        .expect("ETH not in default perp universe") as u32;
    eprintln!("ETH asset index: {}", eth_idx);

    // === best price ===
    let book = client.fetch_book_snapshot(&Symbol::new("ETH")).await
        .expect("fetch ETH book");
    let best_bid = book.best_bid().expect("ETH bid present");
    let order_px = best_bid * PRICE_OFFSET_RATIO;
    let notional = order_px * ORDER_SZ_ETH;
    assert!(notional < MAX_NOTIONAL_USD,
        "ASSERT: notional {} >= max {}", notional, MAX_NOTIONAL_USD);
    eprintln!("ETH best_bid={}, order_px={}, notional≈${}", best_bid, order_px, notional);

    // === place ===
    let cloid = Cloid::new();
    let intent = OrderIntent {
        cloid,
        symbol: Symbol::new("ETH"),
        asset: eth_idx,
        side: Side::Long,
        px: order_px,
        sz: ORDER_SZ_ETH,
        tif: Tif::Alo,
        reduce_only: false,
    };
    let place_resp = client.place_orders(&[intent.clone()]).await
        .expect("place_orders ERR");
    assert_eq!(place_resp.len(), 1);
    let pr = &place_resp[0];
    eprintln!("PLACE: status={}, oid={:?}, error={:?}", pr.status, pr.oid, pr.error);

    // r/s padding 検証 (placement が成功すれば accept された証拠)
    if pr.status == "error" {
        let msg = pr.error.as_deref().unwrap_or("");
        if msg.to_lowercase().contains("signature") {
            panic!("SIGNATURE REJECTED — likely r/s padding issue. msg={msg}");
        }
        panic!("place rejected with non-signature error: {msg}");
    }
    if pr.status == "filled" {
        panic!("UNEXPECTED FILL — manual recovery required (oid={:?})", pr.oid);
    }
    assert_eq!(pr.status, "resting", "expected resting, got {}", pr.status);
    let oid = pr.oid.expect("resting must have oid");

    // brief wait so HL has time to reflect the open order
    tokio::time::sleep(Duration::from_millis(200)).await;

    // === cancel ===
    let cancel = CancelIntent {
        symbol: Symbol::new("ETH"),
        asset: eth_idx,
        by_cloid: Some(cloid),
        by_oid: None,
    };
    let cancel_resp = client.cancel_orders(&[cancel]).await
        .expect("cancel_orders ERR");
    assert_eq!(cancel_resp.len(), 1);
    let cr = &cancel_resp[0];
    eprintln!("CANCEL: status={}, error={:?}", cr.status, cr.error);
    assert_eq!(cr.status, "cancelled", "expected cancelled, got {}", cr.status);

    // === post-snapshot ===
    let post_default = client.fetch_account_state(&master, None).await
        .expect("fetch default state post");
    let post_xyz = client.fetch_account_state(&master, Some("xyz")).await
        .expect("fetch xyz state post");
    let post_xyz_orders = client.fetch_open_orders(&master, Some("xyz")).await
        .expect("fetch xyz orders post");

    eprintln!("POST: default positions={}, xyz positions={}, xyz orders={}",
              post_default.positions.len(), post_xyz.positions.len(), post_xyz_orders.len());

    // === diff guard ===
    // HYPE position 同一
    let hype_pre = pre_default.positions.get(&Symbol::new("HYPE"));
    let hype_post = post_default.positions.get(&Symbol::new("HYPE"));
    match (hype_pre, hype_post) {
        (Some(p1), Some(p2)) => assert_eq!(p1.size, p2.size, "HYPE szi changed!"),
        (None, None) => eprintln!("HYPE absent in both pre/post (ok if user closed it)"),
        _ => panic!("HYPE pre/post mismatch (one side absent)"),
    }
    // xyz:META position 同一
    let meta_pre = pre_xyz.positions.get(&Symbol::new("xyz:META"));
    let meta_post = post_xyz.positions.get(&Symbol::new("xyz:META"));
    match (meta_pre, meta_post) {
        (Some(p1), Some(p2)) => assert_eq!(p1.size, p2.size, "xyz:META szi changed!"),
        (None, None) => eprintln!("xyz:META absent in both pre/post"),
        _ => panic!("xyz:META pre/post mismatch"),
    }
    // xyz:GOOGL open order の oid 同一
    let googl_pre: Vec<u64> = pre_xyz_orders.iter()
        .filter(|o| o.symbol.as_str() == "xyz:GOOGL")
        .map(|o| o.oid.0).collect();
    let googl_post: Vec<u64> = post_xyz_orders.iter()
        .filter(|o| o.symbol.as_str() == "xyz:GOOGL")
        .map(|o| o.oid.0).collect();
    assert_eq!(googl_pre, googl_post, "xyz:GOOGL open orders changed!");

    eprintln!("✓ mainnet round trip success: place oid={} → cancel cloid={}", oid.0, cloid);
}
```

## 7. 受け入れ基準

- [ ] `HL_TEST_ADDRESS=0xfe3e... HL_AGENT_PK=<from pass> cargo test -p executor-hl --features live live_mainnet_place_cancel -- --nocapture --test-threads=1` 1 回実行で:
  - place response: `{status: "resting", oid: <some>}`
  - cancel response: `{status: "cancelled"}`
  - HYPE / xyz:META / xyz:GOOGL pre/post 完全一致
  - eprintln 末尾に `✓ mainnet round trip success` が出る
- [ ] CI では `--features live` 無しで動かないこと (default off の確認)
- [ ] テスト fixture / 結果ファイルに r/s/v / signature が残っていないこと
- [ ] PR-B2b commit に PK / address (master EOA / agent address) を含めないこと
  - master EOA `0xfe3e...7d2d` は既に PR-A 設計 doc + HANDOFF に書いてあるので追加 OK
  - agent address `0xB2a7...b8c5` も `.env.develop` で gitignore 済 → commit に含めない

## 8. リスク評価

| リスク | 確率 | 影響 | 緩和 |
|---|---|---|---|
| r/s padding reject | 中 | place fail (資金影響なし) | テストで signature error 検出 → 即修正 commit |
| ALO クロス → fill | 低 | 0.001 ETH (~$2) の予期せぬ position | best_bid * 0.99 で物理的に防止. 仮に発生しても size $2 |
| HYPE liquidation buffer 縮小 | 極低 | 不要な margin 圧迫 | $0.20 証拠金, withdrawable $34 から十分 |
| nonce 衝突 | ゼロ | — | 単発テスト |
| HL rate limit (1200/min) | ゼロ | — | 6-7 リクエストのみ |
| cancel fail → 注文残存 | 中 | 残存した場合, ユーザー手動 cancel | テスト fail 即時 abort + log |
| network エラー | 低 | retry なしで test fail | 1 回再実行で OK |

## 9. PR-B2b 後のステップ

PR-B2b で **r/s padding accept 確認 + place/cancel wire 実機検証** が完了したら:

1. PR-C (mainnet 別シンボル ALO post-only with 4 重安全装置 — 親 spec Stage C)
   - 本 PR-B2b は Stage C の前哨戦. PR-C との差分: `executor-server` 経由, `--mainnet-allow-symbols` flag, baseline-diff guard を server 内蔵化, etc.
   - 本 PR でも実投入したのでスコープ重複ありだが, PR-C は **production path** で投入するための infra 整備.
2. asset 動的解決 (`fetch_meta` cache を `executor-server` 起動時)
3. emergency_stop の本格テスト (multi-symbol cancel)
4. WS subscriber 本実装

## 10. 関連リンク

- 親 spec: [`2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md`](2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md)
- PR-B1 spec: [`2026-05-05-pr-b1-eip712-signer-design.md`](2026-05-05-pr-b1-eip712-signer-design.md)
- PR-B2a spec: [`2026-05-05-pr-b2a-place-cancel-with-mock-design.md`](2026-05-05-pr-b2a-place-cancel-with-mock-design.md)
- HL exchange endpoint: <https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint>
