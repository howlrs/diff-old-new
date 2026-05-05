# 引き継ぎメモ: PR-D1 mainnet smoke で発覚した重大バグ (2026-05-05)

> 前回引き継ぎ: [`HANDOFF-2026-05-05.md`](HANDOFF-2026-05-05.md) (PR-A〜PR-D1 完了時点)
> このメモは **PR-D1 mainnet smoke で発覚した致命バグ** と原因, 対応状況, 次の担当者への指示をまとめる.
> 状況: **executor-server 停止中, mainnet 上に意図しない ETH 0.035 long ($83 相当) が残存**.

## 1. 概要

ユーザー要望「\$100 ETH long を post-only maker 約定で累積構築」に応じて PASSIVE_FOLLOW を mainnet で動かしたところ:
- **HL UI 上では注文 (resting + 約定) が出た**
- しかし **algo の `filled_size` は 0 のまま**
- algo は約定検知できず最大サイズ (0.005 ETH) で repost 暴走
- 結果: target 0.005 ETH なのに master EOA に **0.035 ETH long が累積**
- 60s 後に BaselineGuard が ETH 0 → 0.035 違反検知 → auto emergency_stop 発火 (= PR-C3 が正しく機能して被害拡大を止めた)

## 2. 真因 (確定)

**HL の WS `userFills` / `orderUpdates` subscribe で渡す `user` field には master EOA address を渡す必要がある. PR-D1 は agent address を渡していた.**

HL 公式 docs (https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket) より:

> When an agent (API wallet) places orders on behalf of a master account, you must use **the master account's address** in the `user` field for both `userFills` and `orderUpdates` subscriptions. Using the agent's address returns empty results.

つまり:
- subscribe には master address (`0xfe3e32cd...`) を渡すべき
- PR-D1 は `signer.address()` (= agent `0xb2a7..8c5`) を渡していた
- → HL は empty results を返す → userFills frame が一切流れない
- → `state.recent_fills` が空のまま → `drain_new_fills` 0 件
- → algo は約定を見えず repost 暴走

## 3. PR-D1 の他の挙動 (副次的)

| 項目 | 状態 |
|---|---|
| WS 接続自体 (`tokio_tungstenite::connect_async`) | ✅ 成功 (log の `connected` + `ws_message_count=633`) |
| `l2Book` subscribe (symbol で subscribe) | ✅ 流れている (`last_book_update` が更新されている) |
| `userFills` / `orderUpdates` subscribe | ❌ **address 間違いで empty** |
| WireWsFill decoder, Cloid::try_from の `0x` + 32hex 解析 | ✅ unit test pass, hyphen-less Uuid::parse_str OK |
| BaselineGuard (PR-C3) | ✅ 60s tick で違反検知 → 正しく fire |
| SafetyGate (PR-C2) | ✅ allow=ETH max=$25 で全 place を gate 通過させた |

`top_level_err` 1 行も log にあったが本筋ではない (rate limit か transient HL issue, place の 95%+ は通って約定している事実から推定).

## 4. ユーザー指示で **私 (前任 Claude) が選んだ誤った調査方向**

ecrecover の挙動 (Gemini deep) に引きずられ「PK ↔ env address mismatch かも」と address verification を起動時 fail-fast に追加した. しかし **PK は正しい (HL に登録済 agent)** が結論. address mismatch check は本問題とは無関係.

ただし:
- 追加した PK ↔ env mismatch check (起動時 fail-fast)
- HL `fetch_user_role` probe (起動時 fail-fast)
- `--master-address` ↔ `userRole.master` 整合性 check

これら **3 つの fail-fast はそれ自体は無害かつ運用上有用** (将来のローテーション時の事故防止). ただ本バグの fix とは独立して評価して merge or revert を判断してほしい.

## 5. 修正方針 (次の担当者へ)

### Fix 1 (本命, 最小修正): WS subscribe の user field を master に変更

`executor-hl/src/ws_subscriber.rs`:
- `WsSubscriberConfig` に `master_address: Address` は既に存在 (reconcile 用)
- `send_subscribe_user_fills(ws, agent)` → `send_subscribe_user_fills(ws, master)` に変更
- `send_subscribe_order_updates(ws, agent)` → `send_subscribe_order_updates(ws, master)` に変更

```rust
async fn connect_and_run(...) -> Result<(), HlError> {
    // ...
    // userFills + orderUpdates (per master, per HL spec)
    send_subscribe_user_fills(&mut ws, &cfg.master_address).await?;       // ← agent → master
    send_subscribe_order_updates(&mut ws, &cfg.master_address).await?;    // ← agent → master
    for sym in &cfg.symbols {
        send_subscribe_l2_book(&mut ws, sym).await?;
    }
    // ...
}
```

REST polling fallback も同じ変更:
```rust
async fn poll_user_fills_fallback(rest_client, manager, master, status) -> ... {
    // fetch_user_fills_by_time の引数も master に
    let fills = rest_client.fetch_user_fills_by_time(master, start_ms, None).await?;
}
```

`agent_address` は **subscribe に使わない**ので削除可能だが, 将来的に使う可能性を考えれば WsSubscriberConfig に残しておいても害はない.

### Fix 2 (重要, defense in depth): PASSIVE_FOLLOW の repost in-flight cap

WS が壊れても暴走しないよう, algo 自体に safety:
- `current_quote: Option<(Cloid, Decimal)>` で **resting 1 つだけ追跡している**前提だが, 実は HL に届いて resting している open_orders は cancel されずに新 cloid で重ね place されている可能性がある (cancel が `unwrap` で握りつぶされている)
- `state.open_orders` を見て「自分が出した未約定 cloid の合計 sz > target」なら place 抑止

```rust
// passive_follow.rs::run 内, place 直前
let in_flight: Decimal = {
    let oo = ctx.state.open_orders.read().await;
    oo.values()
        .filter(|o| own_cloids.contains(&o.cloid))
        .map(|o| o.sz - o.filled_sz)
        .sum()
};
if in_flight + remaining > abs_size + Decimal::from_str("0.0001").unwrap() {
    // 重ね place を防ぐ
    tracing::warn!(in_flight = %in_flight, remaining = %remaining, target = %abs_size,
        "passive_follow: would over-place; skipping place this tick");
    continue;
}
```

### Fix 3 (運用, 任意): WS subscribe ack を見て empty 確認

HL は `subscriptionResponse` を返す. もし subscribe が無効な user で empty を生む場合, ack 内に hint がある可能性 (`isSnapshot=true & fills=[]`). 起動 1 分後に「userFills を 1 件も受信していない」+「algo が place した cloid が `recent_fills` に出てこない」を warn ログ出力する diagnostic を追加検討.

### 既存の修正 (前任 Claude が追加, 評価必要)

`executor-server/src/main.rs` real-mode に追加した 3 つの fail-fast:
1. PK ↔ `HL_AGENT_ADDRESS` env 一致検証
2. HL `fetch_user_role(agent)` で agent registered 確認
3. `--master-address` ↔ `userRole.master` 一致検証

→ これらは **commit していない (作業ブランチ `feat/pr-d1-ws-subscriber` は既に merge 済 e6dc5de). 現状は workspace に **dirty な uncommitted changes** として残っている**:

```
$ git status
modified:   executor/crates/executor-server/src/main.rs   # 上記 fail-fast
```

(注: testnet で test 済. PR-D1 の `Eip712AgentSigner` 経由で testnet 起動して fail-fast の動作確認した.)

判断:
- **(a) 残す**: 起動時 verbose check として運用上有用. PR-D2 で fix 1 と一緒に commit.
- **(b) revert**: 本問題の fix と無関係なので分離. 別 PR で別途検討.

(私の推奨は (a). 既に動作検証済かつ運用上の保険になる.)

## 6. 残存する運用課題

### 6.1 mainnet 上の 0.035 ETH long をどうするか
ユーザー指示: 「保持で OK」. このまま放置.
- entry_px ≈ \$2385.21
- unrealized_pnl ≈ -\$0.057 (小さい)
- 将来 close する場合は手動 (HL UI から) または `intent: "close"` で executor 経由 (PR-D1 fix 後)

### 6.2 BaselineGuard が ETH を baseline に含めない件
`baseline_size=1` で startup 時の baseline は HYPE のみ. ETH は 0 として扱われる.
PR-D2 で ETH が allow-list に入っているなら baseline に **0** ではなく **「監視対象外」** とする選択もある:
- 現実装: baseline に無い symbol は size 0 とみなし, 増加すれば fire
- 検討: allow-list 内 symbol は baseline check から除外 (= algo が自由に建てられる) する設定追加

PR-C3 の Q4 は paranoid (= allow-list 関係なく全 symbol を baseline で守る) で確定したが, mainnet 運用では「allow-list 内は algo に任せて baseline 監視除外」が現実的かもしれない.

ただしこれは **algo が動かない (= filled=0 で位置が膨らむ) ときの安全弁** として今回機能した. 結論として PR-C3 spec は妥当だった. **今のまま残す**を推奨.

### 6.3 PASSIVE_FOLLOW の cancel 挙動
`current_quote.take()` で前 resting を cancel しているが, batch 経由で fire-and-forget. cancel 失敗 (= 既に約定 / 既に cancel 済 / network error) を検知していない. → fix 2 と一緒に検討.

## 7. 次の担当者がやること (推奨順)

1. **executor-server 起動して bug の現状再現確認**:
   ```bash
   cd /home/o9oem/workspace/crypto/diff-old-new
   source scripts/load-env.sh
   cd executor
   cargo run --release -p executor-server -- \
     --mode real --base mainnet --mainnet-allow-symbols ETH \
     --mainnet-max-notional-usd 25 \
     --master-address 0xfe3e32cd4443e395ec0400bf828a34309e517d2d
   ```
   現在の修正済 `main.rs` (uncommitted) で fail-fast 3 つが動作するか log で確認.

2. **Fix 1 を適用**: `ws_subscriber.rs` の `connect_and_run` 内で subscribe 対象を `cfg.master_address` に変更. 同様に REST fallback も.

3. **Fix 1 単体で smoke**: 0.001 ETH (= 約 \$2.4) の最小サイズで PASSIVE_FOLLOW. `ws_message_count` 増加 + `apply_fill` log 出現 + `report.filled_size > 0` を確認.

4. **Fix 2 を適用**: in-flight cap を passive_follow.rs に追加. unit test も.

5. **PR-D2 として commit + push + PR + CI green + merge**.

6. **再 smoke (\$10-15 から)**: ユーザー指示通り段階的に.

7. **本命 \$100 build**: smoke OK 後.

## 8. 参考ログ (前回 mainnet smoke 全文)

```
2026-05-05T12:19:45.637805Z  INFO executor_server: safety gate constructed mode=Real allow_symbols=Some({Symbol("ETH")}) max_notional_usd=Some(25)
2026-05-05T12:19:45.827228Z  INFO executor_server: MetaCache built (default dex) symbols=230
2026-05-05T12:19:45.827304Z  INFO executor_server: ws_subscriber: spawning agent=0xb2a764ff2bb2413cf0f9cbfb22dfe44f13fcb8c5 master=0xfe3e32cd4443e395ec0400bf828a34309e517d2d symbols=[Symbol("ETH")] url=wss://api.hyperliquid.xyz/ws
2026-05-05T12:19:45.929099Z  INFO executor_server: BaselineGuard captured master="0xfe3e32cd4443e395ec0400bf828a34309e517d2d" dexes=[None, Some("xyz")] baseline_size=1 poll_secs=60 szi_epsilon=0
2026-05-05T12:19:46.066385Z  INFO executor_hl::ws_subscriber: ws_subscriber: connected, sending subscribe frames
2026-05-05T12:20:45.218092Z DEBUG executor_algo::passive_follow: passive_follow: repost ALO at touch px=2383.2 sz=0.005 cloid=0x019df815662277b2babaa2f3edc3d8d9
... (22 回 repost) ...
2026-05-05T12:21:35.032524Z ERROR executor_hl::batch_sender: flusher: place_orders failed: hl exchange error: Some("top_level_err") User or API Wallet 0xd74d3804efe7f32a11d6c346adec90c5caa40e37 does not exist.
2026-05-05T12:22:32.495171Z ERROR executor_server: BASELINE VIOLATION DETECTED violations=[BaselineViolation { dex: None, symbol: Symbol("ETH"), baseline_szi: 0, current_szi: 0.035, diff: 0.035 }]
2026-05-05T12:22:32.495244Z  WARN executor_server::routes: emergency_stop dispatched operator="baseline_guard" aborted_executions=1 cancelled_orders=1
```

`0xd74d3804..0e37` は ecrecover 結果のたまたまのアドレスで意味なし (Gemini deep の指摘通り signature payload 不整合の symptom). 多数の place は通って約定している (= place_orders は基本動いている).

## 9. 関連ファイル / コミット

- 直近 merge: PR #77 (e6dc5de) feat(executor): PR-D1 — HL WS subscriber + REST polling fallback
- 不具合 file: `executor/crates/executor-hl/src/ws_subscriber.rs::connect_and_run`
- 不具合 line: `send_subscribe_user_fills(&mut ws, &cfg.agent_address)` (& orderUpdates 同様)
- BaselineGuard fire log: 上記 §8 末尾
- Gemini deep review (誤った方向だが過去 record として): /tmp/agent-mismatch-debug.md (未保存) → 公式 docs (gitbook) で master/agent の HL 仕様確認したのが決定打

## 10. 次の担当者への一言

- ユーザーは **時間制約より機能性 / 検証品質** を重視 (「期待しています」「労働させないで」)
- Gemini deep review は **設計判断** に有効だが, **HL 固有仕様** は公式 docs で必ず確認すべき (今回の master/agent 取り違えは Gemini も誤った推論をしていた)
- WS, REST, eip712, MetaCache, SafetyGate, BaselineGuard は個別には正しく動いている. 問題は **PR-D1 の subscribe の 1 行だけ** (master/agent address 取り違え)

完.
