#!/usr/bin/env bash
# deny-pk-read.sh — PreToolUse hook for Read
#
# Claude の Read ツールが PK を含むファイルを読もうとした場合に exit 2 で deny.
#
# 入力 (stdin, JSON):
#   { "tool_name": "Read", "tool_input": { "file_path": "..." } }

set -u

input=$(cat)
fp=$(printf '%s' "${input}" | grep -oP '"file_path"\s*:\s*"\K[^"]*' | head -1 || true)

if [[ -z "${fp}" ]]; then
    exit 0
fi

# block 対象パターン
patterns=(
    '\.password-store/'
    '\.gnupg/'
    '\.env\.[a-zA-Z]*\.local$'
    '/agent-pk\.gpg$'
)

for pat in "${patterns[@]}"; do
    if [[ "${fp}" =~ ${pat} ]]; then
        cat >&2 <<EOF
DENIED: PK-bearing file Read detected.

Matched pattern: ${pat}
file_path:       ${fp}

理由: HL agent wallet PK / GPG keyring を含むファイルは Claude が Read してはなりません.
project CLAUDE.md の "HL agent wallet PK の取り扱い" 節を参照してください.
EOF
        exit 2
    fi
done

exit 0
