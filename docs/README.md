# docs index

このディレクトリは、研究仮説、実装設計、運用手順、検証結果、引き継ぎを分けて保管する。
初見で読む場合は、下の順に辿ると現在地を掴みやすい。

## 最初に読む

| 目的 | ファイル |
|---|---|
| プロジェクト全体像を把握する | [`PROJECT-OVERVIEW.md`](PROJECT-OVERVIEW.md) |
| 現在の残タスクを確認する | [`TODO.md`](TODO.md) |
| 最新の引き継ぎを読む | [`HANDOFF-2026-06-15-v0.5.1.md`](HANDOFF-2026-06-15-v0.5.1.md) |
| v0.5.1 の到達点を確認する | [`RELEASE-NOTES-v0.5.1.md`](RELEASE-NOTES-v0.5.1.md) |

## 設計資料

| 領域 | ファイル | 内容 |
|---|---|---|
| 本流設計 | [`specs/2026-05-04-v3-design.md`](specs/2026-05-04-v3-design.md) | L1/L2/L3、仮説、KPI、データモデル |
| HL 仕様調査 | [`specs/2026-05-04-phase0-spec-notes.md`](specs/2026-05-04-phase0-spec-notes.md) | Hyperliquid / Trade[XYZ] 周辺仕様メモ |
| Rust executor | [`specs/2026-05-04-rust-executor-design.md`](specs/2026-05-04-rust-executor-design.md) | executor の設計仕様 |
| GUI | [`specs/2026-05-04-gui-design.md`](specs/2026-05-04-gui-design.md) | marimo dashboard 設計 |

## 実装・運用資料

| 領域 | ファイル | 内容 |
|---|---|---|
| Rust executor | [`executor/README.md`](executor/README.md) | executor ドキュメントの入口 |
| Executor architecture | [`executor/architecture.md`](executor/architecture.md) | crate 構成とデータフロー |
| REST API | [`executor/api/rest.md`](executor/api/rest.md) | executor-server REST 仕様 |
| WebSocket API | [`executor/api/websocket.md`](executor/api/websocket.md) | execution progress stream |
| Python connector | [`executor/connector/python.md`](executor/connector/python.md) | Python 戦略層からの呼び出し |
| 開発手順 | [`executor/operations/dev-setup.md`](executor/operations/dev-setup.md) | Rust executor の開発・テスト |
| デプロイ | [`executor/operations/deployment.md`](executor/operations/deployment.md) | 本番投入前チェック |
| トラブル対応 | [`executor/operations/troubleshooting.md`](executor/operations/troubleshooting.md) | 既知エラーと対処 |
| HYPE 積立 | [`executor/operations/hype-passive-twap-runbook.md`](executor/operations/hype-passive-twap-runbook.md) | passive post-only TWAP の実行・監視・停止 |
| Public TWAP 観測 | [`operations/hyperliquid-public-twap-monitor.md`](operations/hyperliquid-public-twap-monitor.md) | free official API による銘柄別 TWAP 総額・share・imbalance 推定 |

## 検証・結果

| 領域 | ファイル | 内容 |
|---|---|---|
| Phase 1 report | [`phase-1-report.md`](phase-1-report.md) | データ監査・特徴量・戦略検証のまとめ |
| Data audit | [`audit/`](audit/) | schema / internal / external / quality score |
| KPI | [`kpi/`](kpi/) | K1/K2/K7/K8/K9、fat-tail、regime diff の結果 |

## 引き継ぎ・履歴

| ファイル | 位置づけ |
|---|---|
| [`HANDOFF-2026-05-04.md`](HANDOFF-2026-05-04.md) | Phase 3 executor prototype 近辺 |
| [`HANDOFF-2026-05-05.md`](HANDOFF-2026-05-05.md) | Phase 3.5 / PR-D1 直前 |
| [`HANDOFF-2026-05-05-PR-D1-POSTMORTEM.md`](HANDOFF-2026-05-05-PR-D1-POSTMORTEM.md) | PR-D1 mainnet smoke で発覚した問題の事後分析 |
| [`HANDOFF-2026-05-06-v0.5.0.md`](HANDOFF-2026-05-06-v0.5.0.md) | v0.5.0 完成時点の最新引き継ぎ |
| [`HANDOFF-2026-06-15-v0.5.1.md`](HANDOFF-2026-06-15-v0.5.1.md) | v0.5.1 public TWAP monitor |

## 補助資料

`superpowers/` は PR ごとの計画・設計ログを保管する履歴領域。通常の開発では、上記の本流資料と `TODO.md` を先に読む。
