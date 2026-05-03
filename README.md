# diff-old-new

> Old/New 金融の構造的差分を活用した、繰り返し可能な統計的優位の発見

Hyperliquid (HIP-3 / Trade[XYZ]) 上の **米株指数 perpetual** と **オールド金融 (CME e-mini, NYSE/NASDAQ)** の **Oracle 二重構造** をデータで観測し、**closure (週末・CMEメンテ・祝日) 中の HL 独自価格発見** に由来するアルファを統計的に検証する。

## ステータス
**Phase 1 進行中 (プロトタイプ構築フェーズ、80% 精度)**

- [x] Phase 0: HL 仕様調査 完了 (`docs/specs/2026-05-04-phase0-spec-notes.md`)
- [x] v3 設計 確定 (`docs/specs/2026-05-04-v3-design.md`)
- [ ] L1 Data Ingestion プロトタイプ
- [ ] L2 Feature Engineering プロトタイプ
- [ ] L3 Strategy Interface 骨格
- [ ] Phase 1 KPI 1, 7 (closure / CMEメンテ時 IPDドリフト分布)

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
