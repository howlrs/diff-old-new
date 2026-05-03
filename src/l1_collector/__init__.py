"""L1 Data Ingestion: HL Info/WS API から raw データを Parquet に保存.

責務 (v3 §4.1):
- WS l2book / trades 受信 + シーケンス検査 + gap recovery
- REST poller (meta / metaAndAssetCtxs / fundingHistory)
- Atomic Parquet writer (temp + os.rename)
- Heartbeat / 欠損率 monitor

NOT責務: 価格計算・特徴量計算・戦略判断 (一切やらない).
"""
