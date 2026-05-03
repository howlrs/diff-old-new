# CHANGELOG

## v0.1.0 — 2026-05-04 (Phase 1 完了)

Phase 1 のコア骨格 + 実データ動作確認 + 全 KPI スクリプト実装. tag付きリリース.

### Added
- **L1 Data Ingestion**:
  - WebSocket client (HIP-3 `xyz:` prefix 対応, 自動再接続, 30秒安定後 backoff reset)
  - REST poller (core / xyz dex 双方の metaAndAssetCtxs)
  - Atomic Parquet writer (temp+rename+fsync, ディレクトリ fsync)
  - Gap recovery (Semaphore で REST rate limit 対策, fire-and-forget タスク化)
  - Heartbeat / 欠損率 monitor
  - Graceful shutdown (SIGINT/SIGTERM → 最終 flush 保証)
- **L2 Feature Engineering**:
  - 動的 calendar (pandas_market_calendars 5.3.2 で NYSE 祝日 + early close 自動取得)
  - Regime tagger (R1〜R6 + boundary buffer + DST 対応)
  - IPD calculator + 連続時間 EMA reconstructor (τ=30min, ±50bps clamp)
  - Spread / pair calculator + Engle-Granger cointegration + OU half-life (statsmodels 0.14.6)
  - Gap detector (regime transition 価格ジャンプ)
  - Resilience metric (大口Taker後の板回復時間)
- **L3 Strategy / Backtest**:
  - Strategy ABC (backtest/live 共通基底)
  - マルチポジ・マルチ銘柄 BacktestEngine (per-symbol ledger + 容量制限 + by_symbol breakdown)
  - Cost model (taker + funding 0.5x + slippage entry/exit 別)
  - 戦略 H1 prototype (closure IPD 累積 mean reversion)
  - CLT 95% 信頼区間ベースの採否判定
- **KPI スクリプト** (実データで動作確認):
  - K1: closure 中 IPD 累積ドリフト分布
  - K2: active 開始時 Oracle ワープギャップ
  - K7: CME メンテ時間 IPD 挙動
  - K8: 週末 BTC/ETH vs TradFi IPD 相関
  - K9: 板の Resilience (3514 events 集計済, BTC p95<1s, xyz:SP500 p95=199s)
- **インフラ**:
  - CI workflow (.github/workflows/ci.yml)
  - Pydantic Settings + YAML config + env-var override
  - structlog (JSON line) ロギング
  - GitHub Issue/PR テンプレート, ラベル体系

### Tests
- 31 / 31 pytest pass
- ruff + format clean

### 主要 PR (develop へ merge 済)
- #26: end-to-end L1/L2/L3 prototype skeleton
- #31: HIP-3 dex support + graceful shutdown + CI + KPI K1/K7
- #32: dynamic calendar + cointegration + resilience + KPI K2/K8/K9
- #33: multi-position / multi-symbol backtest engine

### Gemini partner レビュー履歴
6 ラウンドの致命的指摘を全て反映:
1. v1 設計 (oracle 仕様未調査・片張りリスク)
2. v2 設計 (FR 二重支払い・テーマ逸脱)
3. Phase 0 / v3 (CMEメンテ KPI 提案)
4. プロトタイプ (atomic write / async / sequence check 等 4 件修正)
5. HIP-3 (Pydantic 型 / WS ループブロック / 同期 I/O ブロック 致命 3 件修正)
6. PR #32 (KPI 性能問題 / glob クラッシュ / 大口判定ノイズ / 年ハードコード)
