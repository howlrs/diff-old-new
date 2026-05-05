#!/usr/bin/env bash
# deny-pk-access.sh — PreToolUse hook for Bash
#
# Hyperliquid agent wallet PK の流出を防ぐため, Claude が Bash で
# pass show diff-old-new/hl/agent-pk や gpg -d agent-pk.gpg などを
# 実行しようとした場合に exit 2 で deny する.
#
# 入力 (stdin, JSON):
#   { "tool_name": "Bash", "tool_input": { "command": "..." } }
# 出力規約 (Claude Code hooks):
#   exit 0    -> 通過
#   exit 2    -> deny + stderr の内容を Claude に通知
#   その他   -> 通過 (warn 扱い)
#
# テスト:
#   echo '{"tool_input":{"command":"pass show diff-old-new/hl/agent-pk"}}' | ./deny-pk-access.sh; echo "rc=$?"

set -u

input=$(cat)
# command 文字列を抽出 (jq が無い前提で grep -P)
cmd=$(printf '%s' "${input}" | grep -oP '"command"\s*:\s*"\K[^"]*' | head -1 || true)

if [[ -z "${cmd}" ]]; then
    exit 0
fi

# block 対象パターン
patterns=(
    'pass show diff-old-new/hl/agent-pk'
    'pass show .*agent-pk'
    'gpg.*agent-pk\.gpg'
    'gpg.*-d.*\.password-store'
    'cat .*\.password-store'
    'cat .*agent-pk\.gpg'
    'source .*scripts/load-env\.sh'
    '\. .*scripts/load-env\.sh'
    'HL_AGENT_PK='
    '\.env\.[a-zA-Z]*\.local'
)

for pat in "${patterns[@]}"; do
    if [[ "${cmd}" =~ ${pat} ]]; then
        cat >&2 <<EOF
DENIED: HL agent wallet private key access detected.

Matched pattern: ${pat}
Command:         ${cmd}

理由: 本プロジェクトの HL agent wallet PK は ~/.password-store/ で GPG 暗号化保管されており,
Claude (アシスタント) が PK を transcript に流出させないよう project 規約 (CLAUDE.md) で
PK アクセスを禁止しています.

回避策:
  - PK が必要な操作はユーザー本人が別ターミナルで実行してください.
  - read-only / 公開情報のみで完結する処理に書き換えてください.
  - どうしても PK が要る test の場合は scripts/load-env.sh を ユーザー shell で source した上で
    cargo run / pytest を起動し, Claude セッションは結果ログだけ受け取ってください.
EOF
        exit 2
    fi
done

exit 0
