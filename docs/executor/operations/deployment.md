# デプロイ・本番投入チェックリスト

> **このドキュメントは「80 % プロトタイプから本番に進む前の段取り」を整理したもの**。
> 現状の executor は keyless で動く Mock 構成のみ. 本番投入は **Phase 3.5 以降**。
> 詳細な未完項目は [`../status.md`](../status.md) も参照。

## 本番投入前に必ず行うこと

### 1. 鍵管理ブレスト + 実装

現状: `MockSigner` が `Signer` trait の唯一の impl。

必要な作業:
- [ ] HL agent wallet の運用フロー設計 (master EOA / agent wallet 関係, deregister 手順)
- [ ] EIP-712 typed-data 構造の確定 (HL `/exchange` の正規化)
- [ ] `Eip712AgentSigner` 実装 (`secrecy::Secret<...>` で秘密鍵を保持, `zeroize` で drop 時クリア)
- [ ] 鍵注入経路 (env / vault / KMS) を決めて load 関数を追加
- [ ] 失敗時の `HlError::Signature(...)` 経路を test
- [ ] **agent wallet を 1 アルゴリズム = 1 wallet で割り当てる方針を確定**

### 2. RealHlClient の `place_orders` / `cancel_orders` 完成

現状: skeleton のみ. signer + rate_limiter は通すが POST `/exchange` 本体は `Err(HlError::Exchange { message: "not_implemented" })` を返す。

必要な作業:
- [ ] HL `/exchange` の payload schema を確定 (action, signature, nonce, vaultAddress)
- [ ] レスポンス JSON のパース (oid 抽出, error code mapping)
- [ ] 部分約定や resting 順位等の reconcilation 経路 (WS 経由 vs REST polling)

### 3. Real WS subscriber の実装

現状: `WsStateManager` は state 変換ロジックのみ. ネットワーク接続は別 PR。

必要な作業:
- [ ] `tokio-tungstenite` で `wss://api.hyperliquid.xyz/ws` 接続
- [ ] 切断検知 + 指数バックオフ再接続
- [ ] 起動時 + 再接続後 + 5 分ごとに `clearinghouseState` で reconcile
- [ ] `clearinghouseState` JSON の完全パース (現状は最小限)

### 4. 認証レイヤ (Auth)

**Auth は executor-server 自身には組み込まない方針**。前段 reverse proxy で:

- [ ] mTLS or SSO/JWT を強制
- [ ] `X-Operator-ID` をプロキシで付与 (executor-server は信頼し log に残す)
- [ ] rate limit を proxy 側でも入れる
- [ ] CORS 等は proxy で
- [ ] **public へ executor-server を晒さない** (8085 を VPS で外向きに開けない)

### 5. オブザーバビリティ

現状: `tracing` でログのみ。

将来:
- [ ] `tracing-subscriber` の JSON formatter で構造化ログ → Loki/Grafana
- [ ] Prometheus exporter (P50/P99 レイテンシ, batch flush 数, fill 数等)
- [ ] alert: `emergency_stop` 実行時 → Slack/PagerDuty

### 6. サーバ構成

| 項目 | 推奨 | 理由 |
|---|---|---|
| HL ノード距離 | HL validator にネットワーク的に近い VPS (us-east など) | 70ms HyperBFT finality 活用 |
| systemd unit | restart=on-failure, journald 接続 | 単一プロセスで運用 |
| プロセス数 | **agent wallet 数 × algorithm 種別** ぶん起動可 | nonce 競合回避 (per-process atomic counter) |
| TLS | reverse proxy で終端 | executor 自身は HTTP のみ |

## 設計上の制約 (運用前提)

### 制約 1: BatchSender の 100 ms フラッシュ

HL は ≤1 POST /100 ms ガイダンス。BatchSender の `flush_interval` は `BatchSenderConfig::default()` で 100 ms。
変更すると HL が拒否しはじめる可能性 → 触らない。

### 制約 2: cloid 一意性

cloid は uuid v7 (時刻 + random). 1 プロセス内では衝突しない。
複数プロセスでも v7 (random 部分) で実用上衝突しない想定。

### 制約 3: `all_fills` 拡張 (MARKET_MAKE)

長時間 MM では `all_fills: Vec<Fill>` が memory を食う。

運用ルール:
- ~1 時間ごとに execution をローテーション (cancel → 新 exec_id で再起動)
- もしくは disk persist する wrapper を独自に書く

### 制約 4: ExecutionRegistry の TTL なし

完了済み execution も `Arc<RwLock<ExecutionHandle>>` で残り続ける。
24h 連続稼働時は数千件溜まる可能性。

将来作業:
- [ ] `prune_completed(older_than)` を `ExecutionRegistry` に追加
- [ ] 起動時に `--registry-ttl` などで設定可能化

## 投入手順 (Phase 3.5 着手時の予定)

```mermaid
phase 3.5
  ├── (1) 鍵管理ブレスト
  ├── (2) Eip712AgentSigner + secrecy/zeroize
  ├── (3) RealHlClient.place_orders / cancel_orders
  ├── (4) Real WS subscriber + reconcilation
  ├── (5) testnet で smoke (BTC で 0.001 BTC を MARKET / PASSIVE / TWAP / MM 1 round)
  ├── (6) mTLS proxy + observability
  └── (7) main net で micro-pos (0.01 BTC) で慎重に開始
```

## チェックリスト (本番投入直前)

- [ ] 鍵管理: agent wallet が deregister 可能で master EOA と分離されている
- [ ] `secrecy::Secret<...>` で秘密鍵が in-memory 平文で保持されない (gdb で確認)
- [ ] `RealHlClient::place_orders` で **odd nonce** を使い切ったら graceful refresh される
- [ ] testnet で MARKET / PASSIVE / TWAP / MM 各 1 ラウンドを成功
- [ ] `POST /v1/emergency_stop` を testnet で実走 → 全 cancel が走ることを fill log で検証
- [ ] mTLS proxy が `X-Operator-ID` を Auth 結果から自動付与している
- [ ] ログが Loki / Grafana に流れている. emergency_stop の alert ルール設定
- [ ] `ExecutionRegistry` の TTL/prune が CI 範囲で確認済 (将来)
- [ ] CI が green ((cargo+pytest 共に))

## 関連

- [現状ステータス](../status.md)
- [設計仕様 v2](../../specs/2026-05-04-rust-executor-design.md) §キー管理 / §テスト戦略
- [troubleshooting](troubleshooting.md)
