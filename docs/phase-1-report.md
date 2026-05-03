# Phase 1 完了レポート (v0.1.0)

最終更新: 2026-05-04
Tag: `v0.1.0`

## ビジョン (再掲)
オールド金融 (CME/NYSE/NASDAQ) と新金融 (Hyperliquid HIP-3 / Trade[XYZ]) の **構造的接続点と切断点** を観測し、繰り返し可能な統計的優位 (LLN/CLT) を発見・収益化する.

**Phase 0 で確定した戦略の核**:
- Hyperliquid 米株 perp の **Oracle 二重構造**:
  - active session (US株開場) → CME EMM6 直結 (アルファ薄)
  - closure (週末・CMEメンテ・祝日) → HL内部 EMA τ=30min (**独自価格発見**)
- **closure に集中する** ことが本流戦略

## Phase 1 達成事項

### 設計 (Phase 0 + v3 design)
- [x] Hyperliquid 仕様調査完了 ([phase0-spec-notes.md](specs/2026-05-04-phase0-spec-notes.md))
- [x] v3 設計確定 ([v3-design.md](specs/2026-05-04-v3-design.md))
- [x] Gemini partner レビュー 7+ ラウンド (review_log に記録)

### 実装 (PR #26, #31, #32, #33 すべて merge 済)

#### L1: Data Ingestion
- [x] WebSocket client (HIP-3 `xyz:` prefix 対応 + 自動再接続 + 30s 安定後 backoff reset)
- [x] REST poller (core / xyz dex 双方を polling)
- [x] Atomic Parquet writer (temp+rename+fsync, ディレクトリ fsync も)
- [x] Gap recovery (Semaphore で REST rate limit 抵触防止 + fire-and-forget)
- [x] Heartbeat / 欠損率 monitor
- [x] Graceful shutdown (SIGINT/SIGTERM → 最終 flush 保証)
- [x] async I/O 全面化 (write_parquet_atomic を asyncio.to_thread にオフロード)

#### L2: Feature Engineering
- [x] Regime tagger (R1〜R6 + boundary buffer + DST + early close 対応)
- [x] **動的 calendar** (pandas_market_calendars 5.3.2 で NYSE 祝日自動取得)
- [x] IPD calculator + 連続時間 EMA reconstructor (τ=30min, ±50bps clamp)
- [x] Spread / pair calculator + **Engle-Granger cointegration** + OU half-life
- [x] Gap detector (regime transition 価格ジャンプ)
- [x] **Resilience metric** (大口Taker後の板回復時間, K9 KPI 直接決定)

#### L3: Strategy / Backtest
- [x] Strategy ABC (backtest/live 共通基底 — 実装乖離防止)
- [x] **マルチポジ・マルチ銘柄 BacktestEngine** (per-symbol ledger + 容量制限 + by_symbol breakdown)
- [x] Cost model (taker 0.045%×2 + funding 0.5x dampened + slippage entry/exit 別)
- [x] 戦略 H1 prototype (closure IPD 累積 mean reversion)
- [x] CLT 95% 信頼区間ベースの採否判定

#### Cross-cutting
- [x] CI workflow (.github/workflows/ci.yml で ruff + format + mypy + pytest)
- [x] Pydantic Settings + YAML config + env-var override
- [x] structlog (JSON line) ロギング

### KPI スクリプト (実データで動作確認済)

| KPI | 内容 | 状態 |
|---|---|---|
| K1 | closure 中 SP500 IPD 累積ドリフト分布 | ✅ docs/kpi/K1.md |
| K2 | active 開始時 Oracle ワープギャップ | ✅ docs/kpi/K2.md |
| K7 | CMEメンテ時間 IPD 挙動 | ✅ docs/kpi/K7.md |
| K8 | 週末 BTC/ETH vs TradFi IPD 相関 | ✅ docs/kpi/K8.md |
| K9 | 板の Resilience (Capacity) | ✅ **実データで 3514 events 集計** |

#### K9 の実データ結果 (重要)
| 銘柄 | n_events | recovery rate | median | p95 |
|---|---|---|---|---|
| BTC | 2278 | 99.3% | 0.28s | 0.53s |
| ETH | 821 | 97.8% | 0.26s | 0.48s |
| xyz:XYZ100 | 126 | 97.6% | 0.46s | 6.83s |
| xyz:SP500 | 289 | 94.1% | 0.41s | **199.41s** |

**含意**:
- BTC/ETH は **MM 常駐 → 高 Capacity** (大口を出してもインパクト即解消)
- xyz:SP500 は週末 closure で **p95=199秒の板薄期間** が存在 → **戦略 H1 のサイジング上限を直接決定**

### テスト
- 31 / 31 pytest pass (regime / IPD/EMA / cost model / H1 strategy / cointegration / multi-position / shutdown 等)
- ruff + format clean (Phase 1 では mypy は continue-on-error)

## Gemini partner レビュー履歴
6 ラウンドの致命的指摘を全て反映:
1. v1 設計 → Oracle 仕様未調査・片張りリスク
2. v2 設計 → FR 二重支払い・テーマ逸脱
3. Phase 0 / v3 方向性 → "有望な仮説" 評価 + CMEメンテ KPI 提案
4. プロトタイプ最終 QA → atomic write / async / sequence check 等のバグ 4 件修正
5. HIP-3 dex 対応 → Pydantic 型 / WS ループブロック / 同期 I/O ブロック の致命 3 件修正
6. PR #32 review → KPI 性能問題 / glob クラッシュ / 大口判定ノイズ / 年ハードコード を修正

## Phase 1 KPI 採否判定

**v3 設計 §3 の採否基準**:
> K10 (コスト控除後期待値が正) かつ K11 (年間 N≥500 サンプル) を満たす戦略を最低1つ発見

**現状判定**:
- K9 で **Capacity 制約は判明** (xyz:SP500 でサイジング上限あり)
- 1 週間 collect で **K1/K7 の実分布**, **K2/K8 の実サンプル**が取れる予定
- 戦略 H1 を実分布に基づきチューニング → コスト控除後期待値の判定は Phase 1.5 で完了

## 次のフェーズ (Phase 2 / 3)

Phase 2 ブレスト課題:
- 戦略 H2 (Crypto Native 相関) / H3 (CMEメンテ mini-closure) / H4 (active 中 BTC 連動) の追加検証
- LLN/CLT 適用可否のファットテール定量化 (Hill 推定 + QQ プロット)
- Welch t-test による regime 別差の有意性

Phase 3 課題:
- Live Execution Engine ([#27](https://github.com/howlrs/diff-old-new/issues/27))
- EIP-712 hot wallet 鍵管理
- regime境界 -15min での自動ポジ縮小
- キル スイッチ / drawdown 上限

## ガバナンス改善余地
- mypy strict 通過 (Phase 1 では continue-on-error)
- CI に reviewdog 等で警告通知
- データ schema バージョン管理 (HIP-3 仕様変更追従)
- ログローテ (1週間以上の運用想定)

## 参照
- [v3 設計](specs/2026-05-04-v3-design.md)
- [Phase 0 仕様](specs/2026-05-04-phase0-spec-notes.md)
- KPI: [K1](kpi/K1.md) | [K2](kpi/K2.md) | [K7](kpi/K7.md) | [K8](kpi/K8.md) | [K9](kpi/K9.md)
- 主要 PR: #26 (prototype skeleton), #31 (HIP-3 + shutdown + CI), #32 (calendar + cointegration + KPI), #33 (multi-position)
