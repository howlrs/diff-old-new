# diff-old-new

> Old/New 金融の構造的差分を活用した、繰り返し可能な統計的優位の発見

Hyperliquid (HIP-3 / Trade[XYZ]) 上の **米株指数 perpetual** と **オールド金融 (CME e-mini, NYSE/NASDAQ)** の **Oracle 二重構造** をデータで観測し、**closure (週末・CMEメンテ・祝日) 中の HL 独自価格発見** に由来するアルファを統計的に検証する。

## ステータス
**Data audit pipeline 完備 (v0.4.0)** — Phase 1 詳細: [`docs/phase-1-report.md`](docs/phase-1-report.md), 履歴: [CHANGELOG](CHANGELOG.md)

**HL Oracle 信頼性実証**: BTC oracle が外部 CEX (Binance:OKX:Bybit weighted median) と相関 **0.9792**, median diff -0.18bps → 戦略の前提が信頼に足ることを実データで実証.

- [x] Phase 0: HL 仕様調査 完了 (`docs/specs/2026-05-04-phase0-spec-notes.md`)
- [x] v3 設計 確定 (`docs/specs/2026-05-04-v3-design.md`)
- [x] L1 Data Ingestion (HIP-3 dex 対応, graceful shutdown, async I/O)
- [x] L2 Feature Engineering (動的 calendar, cointegration, resilience, **distribution / Hill 推定**)
- [x] L3 Strategy / Backtest (Strategy ABC, マルチポジ engine, cost model, **Parquet 永続化**)
- [x] **戦略 H1 + H2 + H3** prototype (mean reversion / Crypto Native / CMEメンテ)
- [x] KPI K1/K2/K7/K8/K9 + **fat-tail / regime t-test** スクリプト
- [x] **LiveEngine dry-run** (BacktestEngine と同一 Strategy ABC を継承)
- [x] **Analytics dashboard** (marimo + altair, Sharpe/Sortino/DD 等 8 KPI + KPI カード + 自由探索)
- [x] CI (ruff/format/mypy/pytest), **61/61 tests pass**
- [ ] Phase 3 (実発注 + EIP-712 鍵管理 + キル スイッチ)

## Quick start
```bash
pip install -e ".[dev,gui,audit]"                      # 全部入り
python -m src.cli collect                              # L1 データ収集 (Ctrl-C で停止)
python -m src.cli features                             # L2 features 生成
python -m src.cli backtest h1 --symbol xyz:SP500       # L3 backtest (Parquet 自動保存)

# データ信頼性監査
python scripts/audit_quality.py                        # → docs/audit/D_quality_score.md

# GUI
marimo edit notebooks/dashboard.py                     # 編集モード
marimo run  notebooks/dashboard.py                     # 発表モード
```

## アーキテクチャ
3層メダリオン (詳細は [`docs/specs/2026-05-04-v3-design.md`](docs/specs/2026-05-04-v3-design.md) §4):

```
L3: Strategy / Execution  ← cost-aware, backtest と live が同一 Interface 継承
L2: Feature Engineering   ← regime tagger / IPD / EMA / spread (DuckDB + Polars)
L1: Data Ingestion        ← HL Info/WS API → Parquet append-only
```

## なぜ Hyperliquid で米株 perp なのか

Trade[XYZ] が S&P Dow Jones Indices と公式ライセンス契約 (2026-03-18) し、
HIP-3 経由で `SP500`, `XYZ100`, 米個別株27銘柄等を deploy。
Oracle が時間帯で二重構造になっている:

| レジーム | Oracle |
|---|---|
| **active** (US株開場中) | EMM6 (CME e-mini) / SPX cash index に直結 |
| **closure** (週末・CMEメンテ・祝日) | HL内部 EMA + IPD (τ=30min) → **独自価格発見** |

**closure 中こそ「24時間取引可能なTradFi」が真に独立に動く時間帯**。ここに参加者構成由来のアルファが存在する仮説を検証する。

## 戦略仮説 (詳細は v3 設計 §2)
- **H1**: closure 中 IPD 累積ドリフト → mean reversion / 復帰時ワープ取り
- **H2**: closure 中 BTC/ETH ボラ vs 米株 perp の Crypto Native 相関
- **H3**: 毎日 17-18 ET の CME メンテ時間 mini-closure (週末より高頻度・サンプル多)
- **H4**: active 中の Crypto Native 局所相関 (副次)
- **H5**: closure 中 SP500 vs XYZ100 スプレッド divergence (後フェーズ)

## 検証 KPI
v3 設計 §3 を参照。採否判定は **K10 (コスト控除後期待値が正) かつ K11 (年間 N≥500 サンプル)** を満たす戦略を最低1つ発見すること。

## セットアップ (準備中)

```bash
# Python 3.12+
uv sync
# データ収集開始
python -m src.l1_collector
```

## ガバナンス
各層・各セクションの実装前後で **Gemini partner レビュー** を必須化。
レビュー結果は SurrealDB `review_log` に保管。

## ライセンス
Apache-2.0

## 関連ドキュメント
- [v3 Design (本流)](docs/specs/2026-05-04-v3-design.md)
- [Phase 0 仕様メモ](docs/specs/2026-05-04-phase0-spec-notes.md)

## 注記
本プロジェクトは研究・観測を主目的とし、投資助言ではない。
実取引は自己責任で。
