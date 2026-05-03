"""L2 Feature Engineering: data/raw → data/curated/features.

責務 (v3 §4.2):
- regime tagger (R1〜R6 + boundary buffer)
- IPD calculator
- EMA price reconstructor (τ=30min)
- spread / pair calculator
- gap detector
- resilience metric

NOT責務: 戦略シグナル生成・注文 (一切やらない).
"""
