"""diff-old-new: Old/New 金融構造差分活用プロジェクト.

Layer 構成:
- l1_collector: Hyperliquid データ取得 (WS + REST)
- l2_features: 特徴量エンジニアリング (regime / IPD / EMA / spread)
- l3_strategy: 戦略 + バックテスト (cost-aware)

詳細: docs/specs/2026-05-04-v3-design.md
"""

__version__ = "0.1.0"
