# Rust Executor ドキュメント

Hyperliquid (HIP-3 / Trade[XYZ]) への注文執行レイヤ (`executor/` 以下) のドキュメント集。
2026-05-04 に PR-1 〜 PR-8 の 8 PR で **80 % プロトタイプ** が完成した。

> **80 % プロトタイプとは**:
> 鍵管理 (`secrecy` / `zeroize` 統合, 実 EIP-712 signer) と HL への実 POST `/exchange` 以外
> は全て実装済み。`MockHlClient` + `MockSigner` で keyless にエンドツーエンド動作する。

> **次セッション着手時はまずこちら**:
> - 引き継ぎメモ: [`../HANDOFF-2026-05-04.md`](../HANDOFF-2026-05-04.md)
> - TODO チェックリスト: [`../TODO.md`](../TODO.md)

## 主要ドキュメント

| 区分 | ファイル | 内容 |
|---|---|---|
| **設計** | [`../specs/2026-05-04-rust-executor-design.md`](../specs/2026-05-04-rust-executor-design.md) | 設計仕様 v2 (Gemini deep review 反映) — レイテンシ目標, cancel 戦略, nonce 管理, split-lock 等 |
| 全体像 | [`architecture.md`](architecture.md) | crate 構成, データフロー, 責務分担 |
| **API** | [`api/rest.md`](api/rest.md) | REST 7 endpoint の全仕様 |
| | [`api/websocket.md`](api/websocket.md) | `/v1/exec/{id}/ws` Progress イベント |
| **アルゴリズム** | [`algorithms/market.md`](algorithms/market.md) | MARKET — taker IOC + slippage cap |
| | [`algorithms/passive_follow.md`](algorithms/passive_follow.md) | PASSIVE_FOLLOW — maker ALO at touch |
| | [`algorithms/twap.md`](algorithms/twap.md) | TWAP — 時間スライス |
| | [`algorithms/market_make.md`](algorithms/market_make.md) | MARKET_MAKE — target 駆動 2-sided ALO |
| **連携** | [`connector/python.md`](connector/python.md) | Python から executor を呼び出す (`src/executor/`) |
| | [`cli.md`](cli.md) | `executor-cli` バイナリ — 7 サブコマンド |
| **運用** | [`operations/dev-setup.md`](operations/dev-setup.md) | 開発環境 / ビルド / テスト |
| | [`operations/deployment.md`](operations/deployment.md) | 本番投入前チェックリスト |
| | [`operations/troubleshooting.md`](operations/troubleshooting.md) | よくあるエラーと対処 |
| **状態** | [`status.md`](status.md) | 実装済み / 未実装 / 残タスク (Phase 3.5 以降) |

## 30 秒で全体像

```
┌──────────────────────┐                       ┌──────────────────────┐
│ Python 戦略レイヤ      │  HTTP+WS (port 8085) │ executor-server      │
│ (src/l3_strategy/...)│ ────────────────────▶ │ (axum, Rust)         │
│ + src/executor       │                       │                      │
│   (Python connector) │                       │  ├─ OrderRouter      │
└──────────────────────┘                       │  ├─ ExecutionRegistry│
                                               │  └─ AppState 共有    │
                                               │                      │
                                               │  ┌────────────────┐  │
                                               │  │ executor-algo  │  │
                                               │  │ MARKET/PASSIVE │  │
                                               │  │ /TWAP/MM       │  │
                                               │  └─────┬──────────┘  │
                                               │        │             │
                                               │  ┌─────▼──────────┐  │
                                               │  │ executor-hl    │  │
                                               │  │ BatchSender    │  │
                                               │  │ (100ms flush)  │  │
                                               │  │ TokenBucket    │  │
                                               │  │ HlClient       │  │
                                               │  │ Signer         │  │
                                               │  └─────┬──────────┘  │
                                               └────────┼─────────────┘
                                                        │
                                                        ▼
                                              ┌──────────────────┐
                                              │ Hyperliquid REST │
                                              │ POST /exchange   │
                                              └──────────────────┘
```

## 5 分で動かす

```bash
# 1. ビルド
cd executor
cargo build -p executor-server --release

# 2. 起動 (MockHlClient + MockSigner なので鍵不要)
EXECUTOR_BIND=127.0.0.1:8085 cargo run -p executor-server --release

# 3. ヘルスチェック
curl localhost:8085/v1/health | jq

# 4. CLI から発注 (mock なので何も実取引はしない)
cargo run -p executor-cli -- exec --algo market --symbol BTC --intent open --size 0.1
```

詳細は [`operations/dev-setup.md`](operations/dev-setup.md) を参照。

## 設計上の重要ルール

- **責務単一**: 各アルゴリズム = 独立関数。executor 全体 = HL 発注のみ。
- **Cancel 戦略は cloid 一括 cancel**: nonce 無効化は in-flight しか弾かないため板上指値は残る (Gemini レビュー指摘)。
- **`#![forbid(unsafe_code)]`**: workspace 全 crate で適用。
- **`rust_decimal` で価格・サイズ**: f64 sign 精度問題回避。
- **BatchSender は 100 ms フラッシュ**: HL ≦ 1 POST/100ms ガイダンス遵守。
- **Cancel before Abort** (`/v1/emergency_stop`): 板上 order 先に止め, 後で algo 停止。

## テスト集計 (2026-05-04 時点)

| 区分 | 数 |
|---|---|
| Rust unit + integration | 113 |
| Python (CI 対象) | 79 |
| Python (live e2e, marker `live`) | 5 |
| **合計** | **197** |

`cargo fmt` / `cargo clippy -D warnings` / `ruff` / `mypy` / `scripts/check_ci_local.sh` 全てクリーン。
