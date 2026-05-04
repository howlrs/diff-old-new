# TODO (Phase 3.5 以降)

最終更新: 2026-05-04 (PR-1 〜 PR-8 + docs PR-66 完了時点)
詳細な引き継ぎ: [`HANDOFF-2026-05-04.md`](HANDOFF-2026-05-04.md)

## 必須 (本番投入の前提)

- [ ] **A. 鍵管理ブレスト** ⭐ 最優先  
      検討: 鍵保持場所 / agent wallet 構造 / deregister 手順 / メモリ衛生 / rotation policy  
      → `docs/specs/2026-05-MM-key-management-design.md` を起こす  
      Gemini deep レビュー必須

- [ ] **B. `Eip712AgentSigner` 実装** (`executor-hl/src/signer.rs`)  
      - HL `/exchange` の typed-data 構造 (HL python-sdk 0.23.0 がリファレンス)  
      - `secrecy::Secret<...>` で秘密鍵保持  
      - 単体テスト: 既知 nonce + msg → 既知 signature (HL python-sdk と cross-check)

- [ ] **C. `RealHlClient::place_orders` / `cancel_orders` 完成** (`executor-hl/src/hl_client.rs`)  
      - POST /exchange の payload schema 確定  
      - レスポンス JSON parse (oid 抽出, error mapping)  
      - rate-limited 時の `HlError::RateLimited { wait_ms }`

- [ ] **D. Real WS subscriber** (`executor-hl/src/ws_state.rs` の上に新規ファイル想定)  
      - `tokio-tungstenite` で `wss://api.hyperliquid.xyz/ws`  
      - 切断検知 + 指数バックオフ (200ms→30s, 30s 安定で reset)  
      - 起動時 + 再接続後 + 5min 周期で `clearinghouseState` reconcile

- [ ] **E. Auth レイヤ (前段 reverse proxy)**  
      - mTLS or SSO/JWT  
      - proxy で `X-Operator-ID` 自動付与  
      - executor-server を public に晒さない方針を運用 doc 化

- [ ] **F. testnet smoke** (Phase 3.5 完成判定)  
      - MARKET / PASSIVE / TWAP / MM 各 1 ラウンド  
      - `POST /v1/emergency_stop` の testnet 実走

## 推奨 (Phase 3.5 と並行可)

- [ ] **3.1 Python connector の `X-Operator-ID` サポート**  
      `src/executor/client.py` の `ExecutorClient` に `operator_id` パラメータ追加 (PR-8 deferred)

- [ ] **3.2 ExecutionRegistry の TTL/prune** (PR-7 review)  
      `executor-server/src/registry.rs` に `prune_completed(older_than)` 追加

- [ ] **3.3 broadcast 容量を増やす** (PR-7 review)  
      `executor-server/src/routes.rs:103` の `broadcast::channel(256)` を 1024 等へ

- [ ] **3.4 WS 再接続 / Lagged backoff (Python connector)** (PR-8 review)  
      `src/executor/client.py` の `stream()` に再接続ループ追加

- [ ] **3.5 `tick_size` per symbol meta** (PR-4 deferred)  
      `executor-core/src/symbol.rs` に tick_size を持たせて  
      `passive_follow.rs` の `repost_threshold_ticks` を有効化

- [ ] **3.6 MARKET_MAKE の `all_fills` rotation** (PR-6 known limitation)  
      案 A: server 側で execution を ~1h ごとに自動 rotate  
      案 B: `all_fills` を bounded ring buffer 化

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

## 完了済み (この履歴)

- [x] PR-1〜PR-8: Rust executor 80% プロトタイプ (`executor/`)
- [x] PR-66: `docs/executor/` 日本語ドキュメント整備
- [x] HANDOFF-2026-05-04.md: 本ファイル + 引き継ぎメモ
