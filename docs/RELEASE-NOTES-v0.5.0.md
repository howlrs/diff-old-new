# Release Notes — v0.5.0

リリース日: 2026-05-06
タグ commit: `49a0ca5` (`Merge PR-D8: revert PR-D4 apply_fill position extrapolation`)
GitHub Release: https://github.com/howlrs/diff-old-new/releases/tag/v0.5.0
直前タグ: `v0.4.2` (Phase 1.5 CI patch) — 39 commit / 18 merge commit を含む

## 1. マイルストーン

Phase 4 を完成させ、Hyperliquid mainnet 上で executor-server による
**passive_follow ALO maker 累積 build** を 17 round 連続成功で実証した。
v0.4.2 (Phase 1.5 の CI 修正) からの MINOR bump で、Phase 2〜4 の executor 統合と
実運用実証を1リリースに束ねたもの。

## 2. mainnet 実証結果 (2026-05-06)

| 項目 | 値 |
|---|---|
| Master EOA | `0xfe3e32cd4443e395ec0400bf828a34309e517d2d` |
| Agent wallet | `0xb2a7..b8c5` (`diff-new-old_02`, valid until 2026/11/01) |
| 銘柄 | ETH-PERP |
| 開始 / 終了 ETH long size | 0.115 → 0.200 |
| 増分 | +0.085 ETH (17 round × 0.005) |
| 所要 | 約 3 分 38 秒 (16:29:16 → 16:32:54 UTC) |
| round 平均 | ~13 秒 |
| 約定成功率 | 17 / 17 (delta ≈ +0.005 / round) |
| orphan order | 0 件 |
| BaselineGuard 違反 | 0 件 |
| API rate limit hit | 0 件 |
| entry_px 平均 | $2382.38 |

build に使ったクライアント: `/tmp/build_eth_long_v3.py` (v3.2 final)。HL info REST API を
真値ソースとし、各 round で `start_exec` を投げて executor-server を polling、
完了後に master の `clearinghouseState` で delta を確認、orphan check と
emergency_stop を組み込んだ自己防御スクリプト。

## 3. 統合 PR 一覧

### Phase 2〜3.5 (executor real-mode 化)

| PR | 内容 |
|---|---|
| [#69 PR-A](https://github.com/howlrs/diff-old-new/pull/69) | HL mainnet read-only parser (clearinghouseState / openOrders / l2Book / meta / userRole) |
| [#70 PR-B1](https://github.com/howlrs/diff-old-new/pull/70) | `Eip712AgentSigner` — HL python-sdk と byte-identical 10/10 |
| [#71 PR-B2a](https://github.com/howlrs/diff-old-new/pull/71) | `place_orders` / `cancel_orders` + mockito + 10/10 cross-check |
| [#72 PR-B2b](https://github.com/howlrs/diff-old-new/pull/72) | mainnet 1-round-trip place + cancel 実機実証 |
| [#73 PR-C1](https://github.com/howlrs/diff-old-new/pull/73) | `MetaCache` + executor-server real mode 切替 (`--mode mock\|real`, `--base mainnet\|testnet`) |
| [#74 PR-C2](https://github.com/howlrs/diff-old-new/pull/74) | mainnet safety gate (allowlist + size cap) |
| [#75 PR-C3](https://github.com/howlrs/diff-old-new/pull/75) | baseline-diff guard + idempotent emergency_stop |
| [#76 PR-C4](https://github.com/howlrs/diff-old-new/pull/76) | multi-symbol testnet live + Python e2e CI + `X-Operator-ID` |

### Phase 4 (本リリース主部 — mainnet 実発注運用)

| PR | 内容 | merged |
|---|---|---|
| [#77 PR-D1](https://github.com/howlrs/diff-old-new/pull/77) | HL WS subscriber + REST polling fallback (Phase 4 first) | `e6dc5de` |
| PR-D2 | WS subscribe `user` field を master EOA に修正 + 起動時 fail-fast | `08ce0ab` |
| PR-D3 | PASSIVE_FOLLOW in-flight cap (二次防衛) | `cb57e67` |
| PR-D5 | `status_from_wire` の Rejected variant 拡充 (`badAloPxRejected` 等) | `ed98dee` |
| PR-D6 | BaselineGuard で allowlist 内 symbol を除外 | `ed98dee` |
| PR-D7 | ALO price formatting fix (`Decimal::normalize` で trailing zero strip) | `ed98dee` |
| PR-D8 | PR-D4 (apply_fill 経由 position 加算) を revert | `ad268a3` |

### PR-D4 (中間 merge → 後に PR-D8 で revert)

PR-D4 は `apply_fill` で `state.position` を即時加算して 5min reconcile lag を
埋めようとした実装。しかし起動直後 WS `userFills` の snapshot frame が過去 fills を
全部流し込み、reconcile baseline と二重加算する致命バグが live で判明し、
PR-D8 で完全 revert した。lock-order 改善 (`drop(fills)` hardening) のみ残し、
visibility lag 5min は algo 設計 (target_size 絶対値ベース) で容認。

## 4. 既知の限界 (PR-D9 候補 — v0.5.0 では未着手)

| # | 内容 | 影響度 |
|---|---|---|
| 1 | WS `webData2` channel 未 subscribe → `state.position` は 5min reconcile 周期でしか更新されない | 中 (target_size 絶対値ベースで実害なし、ただし観測性は低い) |
| 2 | `registry.list()` が completed entry も含むため `running_executions` が履歴累積になる | 低 (Python script 側で prev_exec の terminal check で運用回避) |
| 3 | `post_info` / `post_exchange` の HTTP 429 を `HlError::RateLimited` に分類していない | 低 (内部 token bucket で予防、本リリース中は 0 hit) |

詳細は [`HANDOFF-2026-05-06-v0.5.0.md`](HANDOFF-2026-05-06-v0.5.0.md) を参照。

## 5. 関連ドキュメント

- 引き継ぎ (v0.5.0 完成時点): [`HANDOFF-2026-05-06-v0.5.0.md`](HANDOFF-2026-05-06-v0.5.0.md)
- PR-D1 ポストモーテム (失敗記録): [`HANDOFF-2026-05-05-PR-D1-POSTMORTEM.md`](HANDOFF-2026-05-05-PR-D1-POSTMORTEM.md)
- 直前の引き継ぎ (Phase 3.5 + PR-D1 直前): [`HANDOFF-2026-05-05.md`](HANDOFF-2026-05-05.md)
- 残課題チェックリスト: [`TODO.md`](TODO.md)
- Phase 1 当初レポート: [`phase-1-report.md`](phase-1-report.md)

## 6. 教訓 (このリリース固有)

CLAUDE.md K1 (Gemini deep 合議) を初期から守るべきだった。Claude 単独判断で
build スクリプトを 4 回破壊し、ユーザーの再三の指摘 (「Gemini に聞いて」) を
経て初めて正解パスに収束した。次リリース以降は **設計影響のある分岐は
最初から `gemini-review.sh deep` を通す** ことを徹底する。
