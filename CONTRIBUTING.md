# Contributing

## Branch Strategy (Feature Branch Workflow)

```
main      ← 常時稼働可能 (Phase完了時のみ merge)
develop   ← 統合・QA環境 (各 feat/fix が直接 PR する先)
feat/issue-{N}-{slug}  ← 新機能
fix/issue-{N}-{slug}   ← バグ修正
docs/{topic}           ← ドキュメントのみ
```

- `main` への直 push 禁止 (PR経由のみ)
- PR は `develop` に向けて作る (Phase完了で `develop` → `main`)
- 命名: `feat/issue-12-l1-ws-client`、`fix/issue-25-ema-edge-case`

## Definition of Done (DoD)

各 Issue / PR は以下を満たす:

1. **コード**: ruff/mypy エラー 0
2. **テスト**: pytest pass、新規ロジックは coverage ≥ 80%
3. **ドキュメント**: 関数 docstring + 該当 spec / KPI への参照
4. **動作確認**: 想定入力で期待出力が出る (実データ or fixture)
5. **Gemini レビュー**: code review 通し、指摘 close
6. **PR レビュー**: 1名以上の approve、squash merge
7. **SurrealDB 記録**: 該当層の知見を knowledge / output_log に保存

## Labels (運用)

### type:*
- `type:feat` 新機能
- `type:fix` バグ修正
- `type:docs` ドキュメント
- `type:test` テスト追加
- `type:chore` その他
- `type:kpi` KPI測定タスク

### layer:*
- `layer:L1` Data Ingestion
- `layer:L2` Feature Engineering
- `layer:L3` Strategy / Execution
- `layer:cross` 横断 (config / ci / docs)

### prio:*
- `prio:P0` ブロッカー / Phase進行に必須
- `prio:P1` 重要だが他で進める間は待てる
- `prio:P2` Nice to have

### status:*
- `status:design` 設計中
- `status:ready` 着手可
- `status:in-progress` 着手中
- `status:review` レビュー中
- `status:blocked` ブロック中

## Gemini Partner Review (必須プロセス)

各層の実装前後で Gemini レビューを必ず通す。
- 設計レビュー: `gemini-review.sh deep` (pro)
- コードレビュー: `gemini-review.sh review` (flash-lite or --pro)
- Issue起票前: `gemini-review.sh issue`
- 同一エラー 3 回: `gemini-review.sh error`

レビュー結果は SurrealDB `review_log` に保管 (透明性担保)。

## Commit Message Style

[Conventional Commits](https://www.conventionalcommits.org/) を採用:

```
<type>(<scope>): <subject>

<body>

Closes #<issue>
Co-Authored-By: <reviewer / pair> <email>
```

例:
- `feat(l1): add WS sequence number check for l2book gap recovery`
- `fix(l2): handle regime boundary edge case for daily CME maintenance`
- `docs(spec): update v3 design with Gemini partner feedback`

## Code Style

- Python 3.12+
- ruff (lint + format)
- mypy strict
- 型ヒント必須
- docstring (Google style)

## Testing

- pytest + pytest-asyncio + hypothesis (property-based)
- L1 / L2 / L3 各層独立にテスト
- 統合テストは `@pytest.mark.slow`
- Live API は `@pytest.mark.live` (CIで skip)

## Knowledge Persistence (SurrealDB)

各 PR merge 後、必ず以下を SurrealDB に記録:
- 設計判断・トレードオフ → `knowledge`
- 実装した機能の使い方 → `output_log`
- Gemini レビュー結果 → `review_log`

`/home/o9oem/workspace/surreal-query.sh` 経由で操作 (curl直叩き禁止)。
