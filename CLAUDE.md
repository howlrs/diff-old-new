# CLAUDE.md — diff-old-new プロジェクト規約

グローバル規約は `~/.claude/CLAUDE.md`, workspace 規約は `~/workspace/CLAUDE.md` を参照。
本ファイルは diff-old-new プロジェクト固有のルールを定義する。

## ⚠ HL agent wallet PK の取り扱い (絶対遵守)

このプロジェクトの Hyperliquid agent wallet private key は
`~/.password-store/diff-old-new/hl/agent-pk.gpg` に GPG 暗号化して保管している。
Claude (アシスタント) は以下を **絶対に** やってはならない。

### 禁止事項

1. **`pass show diff-old-new/hl/agent-pk` を Bash で実行する**
   - パイプして hash や length を取る場合も含めて全面禁止
   - 必要なときはユーザー自身が別ターミナルで実行する
2. **`gpg -d ~/.password-store/diff-old-new/hl/agent-pk.gpg` 等の直接復号**
3. **`Read` ツールで以下のファイルを読む**
   - `.env.*.local` (将来追加される PK 付き env file)
   - `~/.password-store/` 配下の `.gpg` ファイル
   - `~/.gnupg/` 配下
4. **PK 値 (`0x` + 64 hex の文字列) を出力 / echo / コメントに含める**
5. **`scripts/load-env.sh` を `source` する Bash 実行**
   - pinentry が呼べない subprocess では失敗するが、仮に成功すると
     `HL_AGENT_PK` が transcript の env dump 等に流出する可能性がある

### 機械的防御 (PreToolUse hook)

上記は `.claude/hooks/deny-pk-access.sh` (Bash) と `.claude/hooks/deny-pk-read.sh` (Read) で
機械的に block される。Claude が誤って実行しようとしても OS レベルで止まる。

### 例外手順 (PK が必要な実発注テスト等)

Phase 3.5 の実発注 PR 等で PK が要る局面では:

1. ユーザー自身が **別ターミナル**で `source scripts/load-env.sh` を実行
2. その shell で `cargo run --bin executor-server -- --use-real-signer` 等を起動
3. Claude は PK が export された shell には触れず、結果ログだけを受け取る

Claude が直接 `cargo run` する場合は **必ず `MockSigner` 構成**で起動する。

## 関連ファイル

- 公開 metadata: `.env.develop` (PK は含まない、git ignored)
- env loader: `scripts/load-env.sh` (PK は pass-store から取得)
- agent address / valid_until: `.env.develop` の `HL_AGENT_*` 変数
- 旧 agent (revoke 済 2026-05-05): `app.hyperliquid.xyz`
- 新 agent (active): `diff-new-old_02` (`HL_AGENT_ADDRESS=0xB2a7...b8c5`, valid until 2026/11/1)
