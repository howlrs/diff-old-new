#!/usr/bin/env bash
# scripts/load-env.sh — env ローダー (PK は pass-store から取得)
#
# 使い方 (sourcing でしか機能しない):
#   source scripts/load-env.sh           # default = .env.develop
#   source scripts/load-env.sh prod      # .env.prod を読む等の拡張余地
#
# 動作:
#   1. .env.develop の非機密 metadata を export (HL_MASTER_ADDRESS / HL_AGENT_ADDRESS / etc.)
#   2. pass-store から HL_AGENT_PK を取得して export
#      - pass / gpg-agent / pinentry が動かない場合は明示エラーで終了
#   3. PK は標準出力に echo されない (set -x も off)
#
# セキュリティ:
#   - このスクリプト自身は PK を含まない (pass show でのみ取得)
#   - sourcing 元 shell の HL_AGENT_PK 環境変数にだけ展開される
#   - script 実行 (bash scripts/load-env.sh) では子プロセス env で完結し親に export されない
#
# Claude (アシスタント) は Bash でこのスクリプトを source しないこと。
# pinentry が呼べない subprocess では失敗するし、仮に成功すると PK が transcript に残る。

set +x  # PK が xtrace に乗らないように明示 off

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    echo "ERROR: must be sourced, not executed: source scripts/load-env.sh" >&2
    exit 1
fi

env_name="${1:-develop}"
env_file=".env.${env_name}"

if [[ ! -f "${env_file}" ]]; then
    echo "ERROR: ${env_file} not found in $(pwd)" >&2
    return 1
fi

# 非機密 metadata の export
set -a
# shellcheck disable=SC1090
source "${env_file}"
set +a

# pass-store から PK を取得
if ! command -v pass >/dev/null 2>&1; then
    echo "ERROR: 'pass' not installed. Run: sudo apt install pass" >&2
    return 1
fi

if ! pass ls diff-old-new/hl/agent-pk >/dev/null 2>&1; then
    echo "ERROR: pass entry 'diff-old-new/hl/agent-pk' not found." >&2
    echo "  Run: pass insert diff-old-new/hl/agent-pk" >&2
    return 1
fi

# pinentry を呼ぶ (interactive shell のみ成功する)
_pk=$(pass show diff-old-new/hl/agent-pk 2>&1)
_rc=$?
if [[ ${_rc} -ne 0 ]] || [[ -z "${_pk}" ]]; then
    echo "ERROR: pass show failed (rc=${_rc})." >&2
    echo "  - interactive shell から実行していますか?" >&2
    echo "  - gpg-agent / pinentry は動いていますか?" >&2
    unset _pk
    return 1
fi

# 形式チェック: 0x + 64 hex chars
if [[ ! "${_pk}" =~ ^0x[0-9a-fA-F]{64}$ ]]; then
    echo "ERROR: PK format unexpected (length=$(echo -n "${_pk}" | wc -c))" >&2
    unset _pk
    return 1
fi

export HL_AGENT_PK="${_pk}"
unset _pk

echo "OK: env=${env_file} loaded; HL_AGENT_PK exported (length=${#HL_AGENT_PK})"
echo "    HL_MASTER_ADDRESS=${HL_MASTER_ADDRESS}"
echo "    HL_AGENT_ADDRESS=${HL_AGENT_ADDRESS}"
echo "    HL_AGENT_WALLET_NAME=${HL_AGENT_WALLET_NAME}"
echo "    HL_AGENT_VALID_UNTIL=${HL_AGENT_VALID_UNTIL}"
