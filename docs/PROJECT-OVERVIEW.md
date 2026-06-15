# Project overview

最終確認: 2026-06-15

## 目的

`diff-old-new` は、Hyperliquid / Trade[XYZ] の米株指数 perpetual と、CME・NYSE・NASDAQ などのオールド金融市場の時間構造差を使い、繰り返し可能な統計的優位を検証する研究・実装リポジトリ。

中心仮説は、米株市場や CME が閉じている時間帯でも HL 上の TradFi perp は取引され続けるため、closure 中の独自価格発見、IPD drift、crypto-native correlation、CME メンテ時間の mini-closure に検証可能なエッジがある、というもの。

## 現在地

- Python 側は L1 data ingestion、L2 feature engineering、L3 strategy/backtest、audit、GUI dashboard まで実装済み。
- Rust 側は Hyperliquid executor の workspace があり、MARKET / PASSIVE_FOLLOW / TWAP / MARKET_MAKE、REST+WS server、CLI、Python connector を持つ。
- v0.5.0 時点で mainnet 実発注運用に必要な Phase 4 修正は完了済み。
- v0.5.1 で Hyperliquid official free API だけを使う public TWAP monitor を追加。対象銘柄の TWAP 総額、sampled total volume に対する TWAP share、BUY/SELL imbalance を current/past window で比較できる。
- 最新の残タスクは [`TODO.md`](TODO.md) に集約されている。特に build script CLI 化、PASSIVE_FOLLOW offset、symbol scoped emergency stop、webData2 subscribe などが候補。

## 主要ディレクトリ

| パス | 役割 |
|---|---|
| `src/l1_collector/` | Hyperliquid Info / WS API から raw Parquet を収集 |
| `src/l2_features/` | raw data から regime、IPD、spread、distribution などの特徴量を生成 |
| `src/l3_strategy/` | Strategy interface、backtest、live dry-run、永続化 |
| `src/audit/` | schema / internal consistency / external benchmark / quality score |
| `src/gui/` | marimo dashboard 向け data access、charts、metrics |
| `src/executor/` | Rust executor-server を叩く Python connector |
| `executor/` | Rust workspace。order execution layer 本体 |
| `scripts/` | KPI、audit、HL dry-run、mainnet 補助 CLI などの運用スクリプト |
| `notebooks/` | marimo dashboard |
| `config/` | default/local config |
| `data/` | raw / curated Parquet データ |
| `docs/` | 設計、運用、検証、引き継ぎ資料 |
| `tests/` | Python unit / integration tests |

## Python package map

| Package | 主な入口 |
|---|---|
| `src.cli` | `diff-old-new` / `python -m src.cli` の Typer CLI |
| `src.config` | YAML config loader |
| `src.l1_collector.runner` | L1 collection runner |
| `src.l2_features.pipeline` | feature generation pipeline |
| `src.l3_strategy.backtest` | backtest engine |
| `src.l3_strategy.live` | live dry-run engine |
| `src.executor.client` | executor-server Python client |

CLI の主要コマンド:

```bash
python -m src.cli collect
python -m src.cli features
python -m src.cli backtest h1 --symbol xyz:SP500
python -m src.cli live h1
```

## Rust workspace map

| Crate | 役割 |
|---|---|
| `executor-core` | intent、symbol、state、cloid、nonce、共通型 |
| `executor-hl` | Hyperliquid client、signer、wire schema、WS subscriber、rate limiter |
| `executor-algo` | MARKET / PASSIVE_FOLLOW / TWAP / MARKET_MAKE |
| `executor-server` | axum REST+WS server、registry、router、safety |
| `executor-cli` | executor 操作用 CLI |

詳細は [`executor/README.md`](executor/README.md) と [`executor/architecture.md`](executor/architecture.md) を参照。

## データフロー

```text
Hyperliquid Info/WS
  -> src/l1_collector
  -> data/raw/*.parquet
  -> src/l2_features
  -> data/curated/features/*.parquet
  -> src/l3_strategy backtest/live
  -> data/curated/backtest_results/*.parquet
  -> notebooks/dashboard.py
```

実発注系は別レーンで、Python strategy / script から `src/executor/client.py` を通じて Rust `executor-server` に HTTP+WS で接続し、executor が Hyperliquid へ注文を出す。

## まず動かす

Python:

```bash
pip install -e ".[dev,gui,audit]"
python -m src.cli collect
python -m src.cli features
python -m src.cli backtest h1 --symbol xyz:SP500
marimo run notebooks/dashboard.py
```

Rust executor mock:

```bash
scripts/run-executor-server.sh --mock
cd executor && cargo run -p executor-cli -- health
```

Real mode と鍵管理は [`HANDOFF-2026-05-06-v0.5.0.md`](HANDOFF-2026-05-06-v0.5.0.md) と [`executor/operations/deployment.md`](executor/operations/deployment.md) を確認してから扱う。

共通 env を確認してから起動する場合:

```bash
scripts/executor-env.sh
scripts/run-executor-server.sh --real
scripts/run-executor-script.sh --real scripts/close_passive.py --symbol HYPE --slice-size 1 --slice-times 1
```

Public TWAP monitor:

```bash
.venv/bin/python scripts/hl_public_twap_monitor.py \
  --coin HYPE \
  --user-sample \
  --discover-users recent-trades \
  --sample-rank-by target_volume \
  --sample-top-n 50 \
  --sample-concurrency 1 \
  --sample-report-mode all
```

詳細は [`operations/hyperliquid-public-twap-monitor.md`](operations/hyperliquid-public-twap-monitor.md) を参照。

## 注意点

- `.env.develop`、`scripts/load-env.sh`、秘密鍵関連の扱いは慎重に行う。既存の hook と CLAUDE.md の運用ルールを確認する。
- `data/`、`logs/`、`executor/target/`、各種 cache は作業生成物を含む。資料化やレビュー時はソース・docs・tests と分けて見る。
- HL 残高 API の `withdrawable=$0` だけで資金不足と判断しない。詳細は [`HANDOFF-2026-05-06-v0.5.0.md`](HANDOFF-2026-05-06-v0.5.0.md) の学習事項を参照。
- WS `userFills` snapshot を position に二重加算しない。PR-D8 の背景は [`HANDOFF-2026-05-05-PR-D1-POSTMORTEM.md`](HANDOFF-2026-05-05-PR-D1-POSTMORTEM.md) と最新 handoff を参照。

## 次に見る場所

1. 目的と現状: `README.md`、このファイル
2. 最新タスク: [`TODO.md`](TODO.md)
3. 実発注・運用の文脈: [`HANDOFF-2026-05-06-v0.5.0.md`](HANDOFF-2026-05-06-v0.5.0.md)
4. Public TWAP 観測: [`HANDOFF-2026-06-15-v0.5.1.md`](HANDOFF-2026-06-15-v0.5.1.md)
5. 詳細設計: [`specs/2026-05-04-v3-design.md`](specs/2026-05-04-v3-design.md)
6. executor 詳細: [`executor/README.md`](executor/README.md)
