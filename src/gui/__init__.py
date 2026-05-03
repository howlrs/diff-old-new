"""GUI 用ロジック (data access + perf metrics + charts).

notebooks/dashboard.py が import する. marimo / altair 自体には依存せず,
data_access は DuckDB + Polars, perf_metrics は純粋数値計算, charts は altair.
"""
