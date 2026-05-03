# Audit-B: external benchmark report

Gemini partner 最優先項目: HL Oracle が外部市場と一致しているかの実証.

| symbol | benchmark | n_aligned | corr | median diff bps | p95 abs diff bps | max abs diff bps |
|---|---|---|---|---|---|---|
| BTC | binance+okx+bybit weighted median | 182 | +0.9792 | -0.18 | 2.82 | 14.19 |
| xyz:SP500 | SPY (yfinance) | 0 | - | - | - | - |

### xyz:SP500 notes
- no aligned data

## 解釈ガイド

- **corr ≈ 1.0**: HL Oracle が外部市場と整合している (健全)
- **median diff bps**: 0 周辺なら系統バイアス無し. ±10 bps 程度は active 中の遅延と整合
- **p95 abs diff bps**: closure 中はHL内部 EMA で乖離するので大きくて正常. active のみで filter すると本物の市場差が見える
- **max abs diff bps**: 一時的な outlier (relayer 停止, BTC 急変動 etc)

## 本 audit の限界

- yfinance の SPY 1分足は 7 日前まで. 長期運用では Polygon 等への切替を検討
- weighted median は HL の正式 oracle 計算を完全再現していない (validator stake-weight 部分は不明)
- 1 分 floor アライメント. ms 級ズレは検出不可