# CHANGELOG

## v0.5.1 — 2026-06-15 (Public TWAP monitor)

Hyperliquid official free API だけで、対象銘柄に対する不特定多数の TWAP 実行量をサンプル推定する監視スクリプトを追加。

### Added

- `scripts/hl_public_twap_monitor.py`
  - `recentTrades` から対象銘柄の直近アクティブユーザーを発見。
  - `userTwapSliceFills` で selected users の TWAP slice fills を集計。
  - `userFills` で selected users の対象銘柄全約定量を集計。
  - 直近 1 時間 window と 12 時間前の 1 時間 window を比較。
  - `--sample-rank-by`: `target_volume` / `target_fills` / `all_volume` / `all_fills` / `account_value` / `input_order`。
  - `--sample-report-mode`: `twap-notional` / `twap-share` / `all`。

### Docs

- `docs/operations/hyperliquid-public-twap-monitor.md`
- `docs/RELEASE-NOTES-v0.5.1.md`
- `docs/HANDOFF-2026-06-15-v0.5.1.md`

### Verification

- `.venv/bin/python -m py_compile scripts/hl_public_twap_monitor.py`
- `.venv/bin/ruff check scripts/hl_public_twap_monitor.py`
- official free API で `twap-notional` / `twap-share` / `all` の小サンプル実行確認。

## v0.4.0 — 2026-05-04 (Data audit pipeline — 信頼性監査の体系化)

戦略の前提となる **HL Oracle / 取得データの信頼性** を 4 層で体系的に監査する仕組み.
Gemini partner と相談し推奨順序 A→B→D で実装.

### Added
- **`src/audit/schema_check.py` (Audit-A0)**: 全 Parquet の datetime tz-aware / 必須カラム / dtype 確認
- **`src/audit/internal_consistency.py` (Audit-A)**: recv_ts vs exchange_ts ドリフト, WS gap, mid jump, oracle vs mid, 板健全性
- **`src/audit/external_benchmark.py` (Audit-B, Gemini最優先)**:
  - BTC/ETH oracle vs Binance+Bybit+OKX weighted median (公式 weight 3:2:2 再現)
  - xyz:SP500 oracle vs SPY (yfinance, active session のみ)
  - 1分粒度 floor アライメント (Gemini指摘の罠回避)
- **`src/audit/quality_score.py` (Audit-D)**: 0-100 score (internal 70 + external 30) + closure 期待値ロジック
- **scripts/audit_*.py (4 entry points)** + `docs/audit/{A0,A,B,D}.md` 自動生成
- **marimo dashboard 統合**: Audit セクションで現在の data quality を一目表示
- **`pyproject.toml` audit extra**: yfinance + pytz

### 実 audit 結果 (3h 蓄積データ)
- BTC: 100.0 ✓ (相関 0.9792 vs CEX, median diff -0.18bps → **HL Oracle 信頼性実証**)
- ETH: 100.0 ✓
- xyz:SP500/XYZ100: 95.0 ✓ (週末で SPY 休場, 期待動作)
- internal: latency median 343ms / p99 785ms 全銘柄健全
- 価格ジャンプ 0, 板 crossed 0 → データ品質高い

### Tests
- 71 / 71 pytest pass (新規 10 audit tests)
- ruff / format clean

### 主要 PR
- PR #50: data audit pipeline (A0+A+B+D)

## v0.3.0 — 2026-05-04 (Analytics dashboard — marimo + altair)

戦略 backtest と KPI を 1 画面で対話的に観測する **marimo ベース GUI** を導入.

### Added
- **`src/gui/` モジュール** (PR-B)
  - `data_access.py`: DuckDB in-memory connection ラッパー + 安全な glob 読み込み
  - `perf_metrics.py`: Sharpe (95% CI 付き) / Sortino / Max DD / Hit Rate / Profit Factor / Calmar / Expectancy / Equity curve / Underwater curve
  - `charts.py`: altair 6.1.0 ベース (equity / underwater / trade pnl histogram / IPD time series / top-of-book)
- **`notebooks/dashboard.py`** (PR-C)
  - marimo 0.23.4 reactive notebook
  - 上段: 戦略選択 + 銘柄選択 + Sharpe 等 8 標準 KPI + Equity/Underwater chart + Trade table + PnL ヒストグラム
  - 中段: 既存 KPI (K1/K2/K7/K8/K9/fat_tail/regime_diff) のカード型サマリ
  - 下段: 自由探索セル (IPD time series / top-of-book / DuckDB SQL)
- **backtest 結果 Parquet 永続化** (PR-A)
  - `src/l3_strategy/persistence.py`: `data/curated/backtest_results/{strategy}/{date}/{hour}/` に保存
  - `cli.py backtest --save` (default True) で自動保存

### Tests
- 61 / 61 pytest pass
- ruff / format clean
- `marimo run --headless` で HTTP 200 起動確認

### Usage
```bash
pip install -e ".[dev,gui]"   # marimo + altair をインストール
python -m src.cli backtest h1 --symbol xyz:SP500   # backtest 実行 (Parquet 自動保存)
marimo edit notebooks/dashboard.py                  # 編集モード
marimo run notebooks/dashboard.py                   # 発表モード (read-only アプリ)
```

### 主要 PR (develop merge 済)
- PR-A: backtest result Parquet 永続化
- PR-B: src/gui/ implementation
- PR-C: marimo notebook + docs

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
