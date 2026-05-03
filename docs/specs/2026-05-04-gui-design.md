# GUI Design — Phase 1.5 analytics dashboard (marimo + altair + DuckDB)

作成日: 2026-05-04
バージョン: v1.0 (確定)
状態: 設計確定, 実装着手可

---

## 0. ビジョン

取得済み Parquet データ (L1 raw + L2 curated + KPI json) を **対話的に可視化・分析** する GUI を [marimo](https://marimo.io) で構築する.

v3 design §0.2 「**大数上優位**」「**真の貢献**」の検証ツールとして, 戦略 (H1/H2/H3) のパフォーマンス指標 (Sharpe / Sortino / Max DD ほか) と既存 KPI (K1〜K9 + fat-tail + regime t-test) を1画面で確認できる状態にする.

---

## 1. 要件

### 1.1 利用シーン (確定)
- **C: 探索 + ダッシュボード兼用** — 上段は固定ダッシュボード (戦略 KPI 8指標 + KPI カード), 下段は自由探索セル
- 1ファイルで両用 (`marimo edit` 探索モード / `marimo run` 発表モード)

### 1.2 必須パフォーマンス指標 (確定: 標準8指標)
| 指標 | 表示形式 |
|---|---|
| Sharpe (annualized) | 数値 + 95% CI |
| Sortino | 数値 |
| Max Drawdown | 数値 + Underwater curve |
| Hit Rate | パーセンテージ |
| Profit Factor | 数値 |
| Expectancy bps | 既存 backtest 出力を流用 |
| Equity curve | 累積 P/L 時系列 |
| Trade Table | 個別取引 (entry/exit/symbol/PnL) ソート可 |

加えて以下を補助表示:
- Calmar (Max DD と等価情報だが算出簡単なので併記)
- Regime breakdown (regime 別 Sharpe / Hit / Expectancy)
- Capacity 試算 (K9 連動)

### 1.3 データソース (確定)
**B: DuckDB クエリレイヤー**.
- 既存 `src/l2_features/loader.py` が DuckDB 利用済み, パイプライン統一
- marimo セルから `con.execute("SELECT ...").pl()` で SQL 直書き → 自由探索パートでアドホック分析しやすい
- 起動時に in-memory DB へ Parquet を attach

### 1.4 リアクティブ性
- ウィジェット (戦略 dropdown / 銘柄 dropdown / 期間 slider / regime checkbox) の値が変わると依存セルが自動再評価
- ファイル更新は手動 reload (Phase 1.5 では十分)
- 将来 file watcher / SurrealDB 経由のライブ更新を別フェーズで検討

---

## 2. アーキテクチャ

```
┌──────────────────────────────────────────────┐
│  notebooks/dashboard.py (marimo notebook)    │
│  - reactive cells                             │
│  - 上段: 固定ダッシュボード                    │
│  - 中段: KPI カードビュー                      │
│  - 下段: 自由探索セル                          │
└────────────────────┬─────────────────────────┘
                     │ DuckDB con (in-memory)
                     ↓
   ┌─────────────────┴─────────────────┐
   │  src/gui/  (新規)                  │
   │  - data_access.py: DuckDB ラッパー │
   │  - perf_metrics.py: Sharpe / DD…  │
   │  - charts.py: altair チャート関数 │
   └─────────────────┬─────────────────┘
                     ↓
       data/raw/*.parquet
       data/curated/*.parquet (features, backtest_results)
       docs/kpi/*.json
```

### 2.1 marimo を採用する理由
- **reactive**: ウィジェット値変更 → 依存セル自動再評価. Streamlit の rerun 全体実行と違い細粒度
- **Pure Python file**: git-friendly (Jupyter ipynb のメタデータ汚染なし)
- **2モード共用**: `marimo edit` (探索) と `marimo run` (アプリ) を 1 ファイルで切替
- **WASM export 可**: 将来 GitHub Pages デプロイ可
- **SQL cell built-in**: `marimo[recommended]` で SQL セル直接サポート

### 2.2 altair を採用する理由
- marimo 公式推奨 (`mo.ui.altair_chart()` で統合)
- declarative syntax で短い
- 軽量 (依存は narwhals + jsonschema + jinja2)
- インタラクティブ (zoom/pan/tooltip) 標準装備
- vegafusion で大データ対応も可能 (Phase 2 以降)

---

## 3. 画面構成

### 3.1 ヘッダ + グローバル controls
```
[戦略 ▾ H1] [銘柄 ▾ xyz:SP500] [期間 ━━━━━]
[regime: ☑R1 ☑R2 ☑R3 ☑R4]
```

### 3.2 上段: 固定パフォーマンスダッシュボード
```
┌─ Performance summary (戦略×銘柄×期間) ─────┐
│ Sharpe: 1.2 (95% CI [0.4, 2.0])              │
│ Sortino: 1.8                                 │
│ Profit Factor: 1.6                           │
│ Hit Rate: 58%                                │
│ Expectancy: +12.3 bps                        │
│ Max DD: -3.2%   Calmar: 0.4                  │
└──────────────────────────────────────────────┘
┌─ Equity curve (cumulative net P/L) ─────────┐
│ [altair line chart, regime 帯で色分け]       │
└──────────────────────────────────────────────┘
┌─ Underwater curve (drawdown over time) ─────┐
│ [filled area chart, max DD ハイライト]       │
└──────────────────────────────────────────────┘
┌─ Regime breakdown (table) ──────────────────┐
│ regime  | n  | sharpe | hit% | expectancy   │
│ R1      | 12 | 0.8    | 50%  | +5 bps       │
│ R2      | 30 | 1.5    | 60%  | +18 bps      │
└──────────────────────────────────────────────┘
┌─ Trade table (sortable) ────────────────────┐
│ entry_ts | exit_ts | sym | side | pnl_bps   │
└──────────────────────────────────────────────┘
```

### 3.3 中段: KPI カード
docs/kpi/*.json をカード化. クリックで md 詳細展開:
```
┌─[K1]─────┐ ┌─[K2]─────┐ ┌─[K7]─────┐
│ closure  │ │ warp gap │ │ CME maint│
│ IPD drift│ │ at open  │ │ IPD      │
│ N=1 ⚠    │ │ N=0 ⚠    │ │ N=0 ⚠    │
└──────────┘ └──────────┘ └──────────┘
┌─[K8]─────┐ ┌─[K9]─────┐ ┌─[fat]────┐ ┌─[regime]─┐
│ BTC corr │ │ Capacity │ │ tail     │ │ Welch t  │
│ closure  │ │ p95 SP500│ │ Hill α   │ │ pairwise │
│          │ │ =199s ⚠  │ │          │ │          │
└──────────┘ └──────────┘ └──────────┘ └──────────┘
```

### 3.4 下段: 自由探索セル
- 空セル + サンプル query (`con.execute("SELECT ... ").pl()`)
- IPD 時系列プロット (銘柄選択)
- 板スナップショット heatmap (時刻選択)
- Cointegration spread の z-score プロット
- マークダウンセルでメモ

---

## 4. パフォーマンス指標の実装 (`src/gui/perf_metrics.py`)

```python
@dataclass
class PerfStats:
    n_trades: int
    total_pnl_usd: float
    sharpe_annualized: float
    sharpe_ci_low: float
    sharpe_ci_high: float
    sortino: float
    calmar: float
    max_drawdown_pct: float
    hit_rate: float
    profit_factor: float
    expectancy_bps: float

def compute_perf_stats(
    trades: list[FilledTrade],
    periods_per_year: int | None = None,
) -> PerfStats: ...

def equity_curve(trades) -> pl.DataFrame:    # ts, cumulative_pnl_usd
def underwater_curve(trades) -> pl.DataFrame:  # ts, drawdown_pct
```

### 4.1 Sharpe の年率化
取引頻度が時間軸で不均一なので, 「**1取引あたり** の bps 平均と σ」を 「**年間試行回数 N**」で年率換算:

```
mean_annual = mean_bps × N
std_annual  = std_bps × sqrt(N)
sharpe      = mean_annual / std_annual = mean_bps / std_bps × sqrt(N)
```

`N` は v3 採否基準 K11 (年間試行回数推定) と整合させる.

### 4.2 Sortino
下方リスクのみで除す:
```
downside_returns = [r for r in returns if r < 0]
downside_std = std(downside_returns)
sortino = mean / downside_std × sqrt(N)
```

### 4.3 Max Drawdown
```
cum = cumsum(pnl)
running_max = cum.cummax()
drawdown = (cum - running_max) / running_max
max_dd = drawdown.min()  # 負値
```

### 4.4 Hit Rate / Profit Factor
- Hit Rate = wins / n_trades
- Profit Factor = sum(wins) / |sum(losses)|

### 4.5 Sharpe 95% CI
標準誤差 = `sqrt((1 + sharpe^2 / 2) / n)` (大標本近似).
95% CI = `sharpe ± 1.96 × se`.

---

## 5. 図表ライブラリ (altair)

```python
def equity_curve_chart(df: pl.DataFrame) -> alt.Chart: ...
def underwater_chart(df: pl.DataFrame) -> alt.Chart: ...
def regime_breakdown_table(df: pl.DataFrame) -> alt.Chart: ...
def ipd_time_series(df: pl.DataFrame, symbol: str) -> alt.Chart: ...
def book_snapshot_heatmap(df: pl.DataFrame, ts) -> alt.Chart: ...
```

すべて `mo.ui.altair_chart()` で marimo 統合可. インタラクティブ操作で表示範囲変更.

---

## 6. ディレクトリ構成

```
diff-old-new/
├── notebooks/
│   └── dashboard.py          ← marimo notebook (1ファイル GUI)
├── src/
│   └── gui/                  ← 新規モジュール
│       ├── __init__.py
│       ├── data_access.py    ← DuckDB con + 主要 SQL
│       ├── perf_metrics.py   ← Sharpe / Sortino / DD など
│       └── charts.py         ← altair チャート関数
├── tests/
│   └── test_perf_metrics.py  ← 数値テスト
└── pyproject.toml             ← marimo + altair を notebook extra に追加
```

---

## 7. backtest 結果の永続化 (前提条件 / PR-A)

現状 `BacktestResult.trades` は CLI の print のみ → GUI で読めない. 小さな修正:

- `src/cli.py` の `backtest` コマンドで `BacktestResult` を Parquet 出力
- パス: `data/curated/backtest_results/{strategy}/{run_id}.parquet`
- カラム: trades 各属性 + run metadata (strategy / symbol / cost params / timestamp)
- GUI はこの Parquet を読む

PR-A としてGUI 実装の前に1度入れる.

---

## 8. 依存追加 (`pyproject.toml`)

```toml
[project.optional-dependencies]
notebook = [
    "marimo[recommended]~=0.23.4",
    "altair~=6.1.0",
    # vegafusion は大データ集計時のみ必要 (Phase 2 で追加)
]
```

最新版は実装着手時に PyPI で再確認 (依存解決 conflict があれば調整).

---

## 9. テスト

- `tests/test_perf_metrics.py`: Sharpe / DD / Hit Rate を手計算と照合 (合成 trades)
  - 既知の trade 列 → 期待 Sharpe / DD を hardcode して検証
  - property: trades が空なら全指標 0.0 (no error)
  - property: trades 全勝なら Profit Factor = inf, Hit Rate = 1.0
- marimo notebook 自体は **smoke test のみ** (GUI なので): `marimo run --headless notebooks/dashboard.py` でエラー無く起動
- altair チャート関数: 戻り値が `alt.Chart` インスタンスかをチェック

---

## 10. PR 段取り

1. **PR-A**: backtest Parquet 永続化 (前提)
   - `src/cli.py`, `src/l3_strategy/backtest.py` 修正
   - 既存 H1 backtest を実行して Parquet が出ることを確認
2. **PR-B**: `src/gui/` 実装 + tests
   - data_access / perf_metrics / charts
   - 全ロジックに unit test
3. **PR-C**: `notebooks/dashboard.py` + 依存追加 + docs
   - marimo notebook
   - CHANGELOG / README に GUI セクション
   - `marimo run --headless` の smoke test を CI に追加

3 PR を別タグ (**v0.3.0**) にまとめてリリース.

---

## 11. Definition of Done

- [ ] PR-A merged: backtest 結果が Parquet に保存される
- [ ] PR-B merged: `src/gui/` の全関数に test, ruff/format/pytest pass
- [ ] PR-C merged: `notebooks/dashboard.py` で全 8 KPI 表示 + KPI カード + 探索セル
- [ ] `marimo edit notebooks/dashboard.py` で起動確認 (実データで動く)
- [ ] CHANGELOG / README に GUI セクション追加
- [ ] tag v0.3.0 + GitHub Release
- [ ] CI green

---

## 12. 将来拡張 (本リリース外)

- file watcher / polling 経由のライブ更新 (collect 稼働中の自動更新)
- WASM export → GitHub Pages デプロイ
- SurrealDB バックエンド (knowledge / output_log と連携)
- vegafusion 統合 (大データ集計)
- Bayesian backtest (uncertainty quantification)
- Parameter sweep grid view (戦略パラメータのヒートマップ)

---

## 13. 参照
- [marimo docs](https://docs.marimo.io)
- [altair docs](https://altair-viz.github.io)
- [v3 design](2026-05-04-v3-design.md) §0.2 (大数上優位の定義)
- [phase 1 report](../phase-1-report.md)
