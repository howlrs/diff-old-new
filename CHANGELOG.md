# CHANGELOG

## v0.2.0 — 2026-05-04 (Phase 2 tooling + LiveEngine dry-run)

Phase 2 KPI ツール + 戦略 H2/H3 + LiveEngine dry-run まで完了. 1週間 collect 後に実分布で全 KPI を実行可能な状態.

### Added
- **Phase 2 KPI ツール (PR #40)**:
  - src/l2_features/distribution.py: Hill 推定 + Shapiro-Wilk + Welch t-test
  - scripts/kpi_fat_tail.py: regime 別ファットテール判定
  - scripts/kpi_regime_diff_test.py: pairwise Welch t-test
  - tests/test_distribution.py: 6 tests (Pareto/normal 判定確認済)
- **戦略 H2 / H3 (PR #41)**:
  - strategies/h2_crypto_native.py: closure 中の BTC リターン同方向ベット
  - strategies/h3_cme_maintenance.py: CME メンテ時間 mini-closure mean reversion
  - tests/test_h2_h3_strategies.py: 6 tests
- **LiveEngine dry-run (PR #42)**:
  - src/l3_strategy/live.py: BacktestEngine と同じ Strategy.on_bar を呼ぶ
  - cli.py に live コマンド (h1 / h3 を選択して dry-run 起動)
  - tests/test_live_engine.py: 3 tests
  - 実発注は Phase 3 で別途対応 (EIP-712 hot wallet 等)

### Tests
- 46 / 46 pytest pass
- ruff + format clean

### Issues closed
すべての Open Issues を close. 次フェーズ用の Issue は Phase 3 ブレストで起票.

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
