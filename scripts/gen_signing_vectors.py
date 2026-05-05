#!/usr/bin/env python3
"""Generate known EIP-712 signing vectors from hyperliquid-python-sdk.

Cross-check fixture for the Rust Eip712AgentSigner (PR-B1).

Usage:
    python3 -m venv /tmp/.venv-hl-sdk
    source /tmp/.venv-hl-sdk/bin/activate
    pip install hyperliquid-python-sdk eth-account msgpack
    python3 scripts/gen_signing_vectors.py > executor/crates/executor-hl/tests/fixtures/signing/known_vectors.json

Source vectors mirror tests/signing_test.py from the SDK master branch.
"""
import json
import sys
import eth_account
from hyperliquid.utils.signing import (
    sign_l1_action,
    order_request_to_order_wire,
    order_wires_to_order_action,
    float_to_int_for_hashing,
)
from hyperliquid.utils.types import Cloid

# Same private key as the SDK's signing_test.py uses.
PK = "0x0123456789012345678901234567890123456789012345678901234567890123"
wallet = eth_account.Account.from_key(PK)

VECTORS = []


def emit(name, action, nonce, vault, expires, is_mainnet):
    sig = sign_l1_action(wallet, action, vault, nonce, expires, is_mainnet)
    VECTORS.append(
        {
            "name": name,
            "action": action,
            "nonce": nonce,
            "vault_address": vault,
            "expires_after": expires,
            "is_mainnet": is_mainnet,
            "expected_r": sig["r"],
            "expected_s": sig["s"],
            "expected_v": sig["v"],
            "expected_address": wallet.address.lower(),
        }
    )


# Vector 1: dummy action
dummy_action = {"type": "dummy", "num": float_to_int_for_hashing(1000)}
emit("dummy_mainnet", dummy_action, 0, None, None, True)
emit("dummy_testnet", dummy_action, 0, None, None, False)

# Vector 2: order
order_request = {
    "coin": "ETH",
    "is_buy": True,
    "sz": 100,
    "limit_px": 100,
    "reduce_only": False,
    "order_type": {"limit": {"tif": "Gtc"}},
    "cloid": None,
}
order_action = order_wires_to_order_action(
    [order_request_to_order_wire(order_request, 1)]
)
emit("order_eth_mainnet", order_action, 0, None, None, True)
emit("order_eth_testnet", order_action, 0, None, None, False)

# Vector 3: order with cloid
order_request_c = {
    "coin": "ETH",
    "is_buy": True,
    "sz": 100,
    "limit_px": 100,
    "reduce_only": False,
    "order_type": {"limit": {"tif": "Gtc"}},
    "cloid": Cloid.from_str("0x00000000000000000000000000000001"),
}
order_action_c = order_wires_to_order_action(
    [order_request_to_order_wire(order_request_c, 1)]
)
emit("order_with_cloid_mainnet", order_action_c, 0, None, None, True)
emit("order_with_cloid_testnet", order_action_c, 0, None, None, False)

# Vector 4: dummy with vault
VAULT = "0x1719884eb866cb12b2287399b15f7db5e7d775ea"
emit("dummy_with_vault_mainnet", dummy_action, 0, VAULT, None, True)
emit("dummy_with_vault_testnet", dummy_action, 0, VAULT, None, False)

# Vector 5: scheduleCancel (basic, no time)
schedule_cancel = {"type": "scheduleCancel"}
emit("schedule_cancel_mainnet", schedule_cancel, 0, None, None, True)
emit("schedule_cancel_testnet", schedule_cancel, 0, None, None, False)

json.dump(VECTORS, sys.stdout, indent=2, ensure_ascii=False)
sys.stdout.write("\n")
