#!/usr/bin/env python3
"""Sanitize a raw /tmp/hl-snapshot-*.json file for use as a test fixture.

Usage:
    python3 scripts/sanitize_hl_fixture.py <input.json> <output.json> <kind>

kind = clearinghouseState | openOrders | l2Book | meta | userRole

Replaces:
- user addresses -> 0x000000000000000000000000000000000000dead
- oids -> sequential 1, 2, 3, ...
- preserves all other field NAMES and TYPES so parser tests are realistic.
- preserves szi/limitPx/sz numeric STRINGS verbatim (the parser is what we test;
  changing numbers would mask formatting bugs).
"""
import json
import re
import sys
from pathlib import Path

SENTINEL_ADDR = "0x000000000000000000000000000000000000dead"


def _scrub_address(value):
    if isinstance(value, str) and re.fullmatch(r"0x[0-9a-fA-F]{40}", value):
        return SENTINEL_ADDR
    return value


def _walk(obj, oid_counter):
    if isinstance(obj, dict):
        out = {}
        for k, v in obj.items():
            if k in ("user", "address", "deployer", "oracleUpdater", "feeRecipient"):
                out[k] = _scrub_address(v)
            elif k == "oid" and isinstance(v, int):
                out[k] = oid_counter[0]
                oid_counter[0] += 1
            elif k == "data" and isinstance(v, dict) and "user" in v:
                out[k] = {**v, "user": _scrub_address(v["user"])}
            else:
                out[k] = _walk(v, oid_counter)
        return out
    if isinstance(obj, list):
        return [_walk(x, oid_counter) for x in obj]
    return obj


def main():
    if len(sys.argv) != 4:
        print(__doc__)
        sys.exit(1)
    in_path = Path(sys.argv[1])
    out_path = Path(sys.argv[2])
    kind = sys.argv[3]
    raw = json.loads(in_path.read_text())
    oid_counter = [1]
    sanitized = _walk(raw, oid_counter)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(sanitized, indent=2, ensure_ascii=False) + "\n")
    print(f"OK: {in_path} -> {out_path} (kind={kind})")


if __name__ == "__main__":
    main()
