# Phase 1 完了レポート (中間 — 2026-05-04)

> このレポートは **Phase 1 のコア骨格完成時点 (2026-05-04)** に作成した中間版.
> 1週間 collect の実データが集まり次第, KPI K1 / K7 の数値結果を更新する.

## 達成済み

### 設計・調査
- [x] Phase 0: Hyperliquid 仕様調査 (`docs/specs/2026-05-04-phase0-spec-notes.md`)
  - HIP-3 dex (Trade[XYZ]) の Oracle 二重構造 (active = CME 直結 / closure = HL 内部 EMA τ=30min) 把握
- [x] v3 設計確定 (`docs/specs/2026-05-04-v3-design.md`)
  - 3層メダリオン (L1 collector / L2 features / L3 strategy)
  - 戦略仮説 H1 (closure mean reversion) / H3 (CMEメンテ ミニ closure) を主軸

### コード (PR #26 + #31, develop merge 済み + 進行中)
- [x] **L1 Data Ingestion**: WS + REST + Parquet 永続化 + graceful shutdown
  - HIP-3 dex 対応 (`xyz:SP500`, `xyz:XYZ100` + core BTC/ETH の同時収集)
  - SIGINT/SIGTERM で最終 flush 保証
  - asyncio.to_thread で I/O オフロード, asyncio.create_task で gap recovery を fire-and-forget
- [x] **L2 Feature Engineering**: regime tagger / IPD / EMA / spread / gap detector
  - DST-aware ET 変換 (pendulum), boundary buffer
- [x] **L3 Strategy / Backtest**: Strategy ABC + cost model + H1 戦略
  - cost に taker (×2) + funding (0.5x) + slippage (entry/exit 別) を完全控除
- [x] **CI workflow**: ruff / format / mypy / pytest on PR (Python 3.12)
- [x] **KPI スクリプト**: K1 (closure IPD ドリフト) / K7 (CME メンテ) の自動レポート生成
- [x] **テスト**: 26 / 26 pytest pass

### 実 API 検証 (dry-run + 1分稼働)
- [x] WS subscribe `xyz:SP500` 等で実データ受信確認
- [x] REST `metaAndAssetCtxs` の dex=xyz / "" の両 polling 成功
- [x] 4 銘柄 (xyz:SP500 / xyz:XYZ100 / BTC / ETH) を 1 分間で 各 112 板 + 100+ trades 取得
- [x] L1 → L2 → L3 backtest の end-to-end パイプライン稼働 (合成データ + 実データ両方)

### Gemini partner プロセス
- [x] v1 / v2 / Phase 0 / v3 / プロトタイプ / HIP-3 修正 で計 6 ラウンドのレビュー
- [x] 致命的指摘 (Oracle 仕様未調査, 片張りリスク, 同期 I/O ブロック, WSループブロック等) を全て反映
- [x] レビュー履歴は SurrealDB `review_log` に保存

## 未達 (1週間 collect 完了で達成予定)

- [ ] **KPI K1 実分布**: closure 中 SP500 IPD 累積ドリフト (週末 R2 / CMEメンテ R3 / 祝日 R4 別)
- [ ] **KPI K7 実分布**: CMEメンテ時間 IPD ドリフト + post-maint open ギャップ
  - 期待: 1週間で R3 セグメント 5 個程度, 週末 1 サイクル, 1 R2 セグメント
  - n=30+ で正規性検定可能なのは 1ヶ月以降
- [ ] **K10 (コスト控除後期待値)**: H1 を実データに対して N≥30 取引で評価
- [ ] **K11 (年間試行頻度推定)**: 蓄積データから年間サンプル N の推定

## 既知の制約 (Phase 1.5+ で対応)

| ID | 内容 | 対応 Issue / 想定 |
|---|---|---|
| C1 | Calendar 静的 (US_EQUITY_HOLIDAYS_2026) | #28 で `pandas_market_calendars` に置き換え |
| C2 | Resilience metric 未実装 | #13 で大口Taker後の板回復時間を実測 |
| C3 | ペア / マルチ銘柄バックテスト未対応 | #29 でマルチポジ engine に拡張 |
| C4 | Live Execution Engine 未実装 | #27 で Strategy ABC 共通継承の証明 |
| C5 | mypy strict 未通過 (continue-on-error) | Phase 2 で必須化 |
| C6 | データ schema バージョン管理なし (Gemini 指摘) | Phase 1.5 で raw に schema_version 追加 |
| C7 | ログローテ無し | systemd service / logrotate 化 (運用整備) |

## 運用注意 (Gemini partner からの 4 提言)

1. **ディスク監視**: 1 日 ~320MB (4 銘柄). 1 週間で ~2GB → 余裕あるが監視追加
2. **NTP 同期**: regime 判定が `exchange_ts` (HL 提供) ベースなのでホスト時刻ずれは直接影響しないが、`recv_ts` ベースの遅延診断には NTP 必須
3. **WS 切断ギャップ**: 自動再接続 + REST snapshot 復旧は実装済. 切断中の数秒〜数十秒のティック欠損は不可避
4. **Schema 変更追従**: HIP-3 仕様変更に備えて raw データに schema_version カラムを追加検討

## 次のフェーズ (Phase 2)

K1/K7 の n_segments ≥ 30 が満たされたら以下に進む:

1. **正規性検定 + Hill 推定**: closure IPD 分布が ファットテール か正規か判定
2. **コスト控除後期待値の確認 (K10)**: 採否判定を満たす戦略が存在するか
3. **戦略 H1 のチューニング**: 実分布に基づいて閾値・サイズ調整
4. **戦略 H2 (Crypto Native 相関) と H3 (CME メンテ) の追加検証**

Phase 2 突入時は別途ブレストで規律設計 (キル スイッチ / drawdown 上限 / hot wallet 鍵管理).

## 参照
- v3 設計: [`docs/specs/2026-05-04-v3-design.md`](specs/2026-05-04-v3-design.md)
- Phase 0 仕様: [`docs/specs/2026-05-04-phase0-spec-notes.md`](specs/2026-05-04-phase0-spec-notes.md)
- KPI: [`docs/kpi/K1.md`](kpi/K1.md), [`docs/kpi/K7.md`](kpi/K7.md)
- PR履歴: #26 (L1/L2/L3 prototype skeleton), #31 (HIP-3 + shutdown + CI + KPI)
