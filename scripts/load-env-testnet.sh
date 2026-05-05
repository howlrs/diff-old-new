#!/usr/bin/env bash
# scripts/load-env-testnet.sh — testnet PK loader (PR-C4).
#
# Mirrors `load-env.sh` but reads from a separate pass-store entry so testnet
# and mainnet keys never share a process. Targets HL Hyperliquid testnet.
#
# Usage (sourcing only):
#   source scripts/load-env-testnet.sh
#
# Setup once:
#   1. Generate a new agent wallet for testnet (1-shot key, never reused on mainnet)
#   2. pass insert diff-old-new/hl-testnet/agent-pk
#      → paste the 0x-prefixed 64-hex private key
#   3. Create `.env.testnet` (git-ignored) with:
#         HL_TESTNET_MASTER=0x...   # public master EOA address
#         HL_TESTNET_AGENT_ADDRESS=0x...
#
# Output:
#   - Exports HL_TESTNET_AGENT_PK (sourced from pass-store)
#   - Exports HL_TESTNET_MASTER (from .env.testnet)
#
# Security: Claude (the assistant) must not source this script via Bash —
# pinentry won't fire in a subprocess and the PK could leak into the transcript.

set +x  # ensure xtrace stays off so PK never appears in shell logs

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    echo "ERROR: must be sourced, not executed: source scripts/load-env-testnet.sh" >&2
    exit 1
fi

env_file=".env.testnet"

if [[ ! -f "${env_file}" ]]; then
    echo "ERROR: ${env_file} not found in $(pwd)" >&2
    echo "  Create it (git-ignored) with at least:" >&2
    echo "    HL_TESTNET_MASTER=0x..." >&2
    return 1
fi

# Non-secret metadata export
set -a
# shellcheck disable=SC1090
source "${env_file}"
set +a

if ! command -v pass >/dev/null 2>&1; then
    echo "ERROR: 'pass' not installed. Run: sudo apt install pass" >&2
    return 1
fi

if ! pass ls diff-old-new/hl-testnet/agent-pk >/dev/null 2>&1; then
    echo "ERROR: pass entry 'diff-old-new/hl-testnet/agent-pk' not found." >&2
    echo "  Run: pass insert diff-old-new/hl-testnet/agent-pk" >&2
    return 1
fi

_pk=$(pass show diff-old-new/hl-testnet/agent-pk 2>&1)
_rc=$?
if [[ ${_rc} -ne 0 ]] || [[ -z "${_pk}" ]]; then
    echo "ERROR: pass show failed (rc=${_rc})." >&2
    echo "  - interactive shell から実行していますか?" >&2
    echo "  - gpg-agent / pinentry は動いていますか?" >&2
    unset _pk
    return 1
fi

if [[ ! "${_pk}" =~ ^0x[0-9a-fA-F]{64}$ ]]; then
    echo "ERROR: PK format unexpected (length=$(echo -n "${_pk}" | wc -c))" >&2
    unset _pk
    return 1
fi

export HL_TESTNET_AGENT_PK="${_pk}"
unset _pk

echo "OK: env=${env_file} loaded; HL_TESTNET_AGENT_PK exported (length=${#HL_TESTNET_AGENT_PK})"
echo "    HL_TESTNET_MASTER=${HL_TESTNET_MASTER:-<unset>}"
echo "    HL_TESTNET_AGENT_ADDRESS=${HL_TESTNET_AGENT_ADDRESS:-<unset>}"
