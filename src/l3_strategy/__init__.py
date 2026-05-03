"""L3 Strategy / Execution: cost-aware backtest + Strategy Interface.

責務 (v3 §4.3):
- StrategyInterface (backtest と live が同一クラス継承 — 実装乖離防止)
- BacktestEngine (Polars DataFrame iterator)
- Cost model (taker + funding + slippage)
- 戦略 H1 (closure IPD累積 mean reversion) prototype

NOT責務: データ取得・特徴量計算 (L1/L2 に委譲).
"""
