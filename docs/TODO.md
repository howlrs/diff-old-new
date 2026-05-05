# TODO (Phase 4 後 / v0.5.0 以降)

最終更新: 2026-05-06 (v0.5.0 タグ付け時点)
リリースノート: [`RELEASE-NOTES-v0.5.0.md`](RELEASE-NOTES-v0.5.0.md)
最新引き継ぎ: [`HANDOFF-2026-05-06-v0.5.0.md`](HANDOFF-2026-05-06-v0.5.0.md)

## v0.5.0 で完了した過去 TODO (記念碑)

Phase 3.5 で必須としていた A 〜 F は全て完了済。

- [x] **A. 鍵管理ブレスト** — `~/.password-store/diff-old-new/hl/agent-pk.gpg` GPG 暗号化保管 + `scripts/load-env.sh` + project CLAUDE.md + PreToolUse hook (PR #68)
- [x] **B. `Eip712AgentSigner`** — alloy 2.0.4 + sol! Agent + msgpack、HL python-sdk と byte-identical 10/10 (PR-B1)
- [x] **C. `RealHlClient::place_orders` / `cancel_orders`** — mockito 9 tests + mainnet 1-round-trip 実機実証 (PR-B2a / PR-B2b)
- [x] **D. Real WS subscriber** — `tokio-tungstenite` + 指数バックオフ + 5min reconcile (PR-D1, master_address fix は PR-D2)
- [x] **E. Auth レイヤ** — `X-Operator-ID` header + Python connector 対応 (PR-C4)
- [x] **F. testnet smoke** — multi-symbol testnet live + Python e2e CI (PR-C4)

## 必須 (PR-D9 候補 — v0.5.0 既知の限界の解消)

- [ ] **D9-1. WS `webData2` channel subscribe**
      `state.position` を 5min reconcile を待たずに即時更新したい。Gemini deep の推奨は webData2 + isSnapshot filter で、起動直後の snapshot frame が過去 fills を二重加算する PR-D4 の轍を踏まないこと。
      影響: 観測性 / build script の delta polling 簡易化

- [ ] **D9-2. `ExecutionRegistry::list()` の running 限定**
      現状 `list()` は completed entry も返すため `running_executions` 数値が履歴累積になる。Python 側で prev_exec の terminal check で運用回避しているが、サーバ側で `status: Running` だけ返すか別 endpoint を切り出すのが本筋。
      影響: 運用回避済 (実害低)、API の自然さ

- [ ] **D9-3. HTTP 429 を `HlError::RateLimited` に分類**
      `post_info` / `post_exchange` で HTTP 429 を network error として扱っている。内部 token bucket で予防しているため v0.5.0 中は 0 hit だが、将来 burst が出たときに正しく backoff したい。
      影響: 本番運用の robustness

## 推奨 (Phase 3.5 から繰り越し、優先度低)

- [ ] **3.2 ExecutionRegistry の TTL/prune** (PR-7 review)  
      `executor-server/src/registry.rs` に `prune_completed(older_than)` 追加 (D9-2 で同時に解決可能)

- [ ] **3.3 broadcast 容量を増やす** (PR-7 review)  
      `executor-server/src/routes.rs:103` の `broadcast::channel(256)` を 1024 等へ

- [ ] **3.4 WS 再接続 / Lagged backoff (Python connector)** (PR-8 review)  
      `src/executor/client.py` の `stream()` に再接続ループ追加

- [ ] **3.5 `tick_size` per symbol meta** (PR-4 deferred)  
      `executor-core/src/symbol.rs` に tick_size を持たせて `passive_follow.rs` の `repost_threshold_ticks` を有効化

- [ ] **3.6 MARKET_MAKE の `all_fills` rotation** (PR-6 known limitation)  
      案 A: server 側で execution を ~1h ごとに自動 rotate / 案 B: `all_fills` を bounded ring buffer 化

## 任意 (応用)

- [ ] **3.7 empty book backoff** (PR-4 review)  
      `market: empty asks (no best ask)` で即 abort せず短時間 retry してから abort

- [ ] **3.8 Pydantic schema 中央化** (PR-8 review)  
      Rust serde ↔ Python の wire 一致を Pydantic + alias_generator で自動化

- [ ] **3.9 観測性 (observability)**  
      - `tracing-subscriber` JSON formatter → Loki/Grafana  
      - Prometheus exporter (P50/P99 レイテンシ, batch flush 数, fill 数)  
      - `emergency_stop` 実行時の Slack/PagerDuty alert

- [ ] **3.10 複数プロセス並列稼働**  
      agent wallet × algorithm 種別ごとに独立プロセス起動

- [ ] **3.11 MARKET の dynamic slippage**  
      直近 fill 結果から slippage を学習

## 完了済み (履歴)

- [x] PR-1〜PR-8: Rust executor 80% プロトタイプ (`executor/`, ~v0.4.x)
- [x] PR-66: `docs/executor/` 日本語ドキュメント整備
- [x] HANDOFF-2026-05-04.md / HANDOFF-2026-05-05.md / HANDOFF-2026-05-05-PR-D1-POSTMORTEM.md
- [x] PR-A 〜 PR-D8: Phase 2〜4 統合 (v0.5.0)
- [x] mainnet passive_follow build 0.115 → 0.200 ETH 実証 (2026-05-06, 17/17 約定成功)
