"""実 Hyperliquid API に L1 を 60秒繋いで挙動確認 (dry run).

目的:
- WS message format 実測 (channel / data / time field の存在確認)
- l2book sequence number の有無確認
- REST metaAndAssetCtxs のレスポンス構造確認
- 各 symbol のメッセージ受信頻度

使い方:
    python scripts/dryrun_hl_api.py [--seconds 60]
"""

from __future__ import annotations

import asyncio
import itertools
import json
import sys
from collections import Counter, defaultdict
from datetime import UTC, datetime
from pathlib import Path

import httpx
import websockets

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))

from src.config import load_config  # noqa: E402
from src.logging_setup import setup_logging  # noqa: E402

DEFAULT_SECONDS = 60


async def ws_probe(url: str, symbols: list[str], seconds: int) -> dict[str, object]:
    """WS に接続して指定秒数メッセージを集計."""
    counts: Counter = Counter()
    seq_seen: dict[str, list[int]] = defaultdict(list)
    samples: dict[str, list[dict]] = defaultdict(list)
    msg_keys: Counter = Counter()
    end_at = asyncio.get_event_loop().time() + seconds

    async with websockets.connect(url) as ws:
        for sym in symbols:
            await ws.send(
                json.dumps(
                    {
                        "method": "subscribe",
                        "subscription": {"type": "l2Book", "coin": sym},
                    }
                )
            )
            await ws.send(
                json.dumps(
                    {
                        "method": "subscribe",
                        "subscription": {"type": "trades", "coin": sym},
                    }
                )
            )

        try:
            while asyncio.get_event_loop().time() < end_at:
                remaining = end_at - asyncio.get_event_loop().time()
                if remaining <= 0:
                    break
                try:
                    raw = await asyncio.wait_for(ws.recv(), timeout=remaining)
                except TimeoutError:
                    break
                msg = json.loads(raw)
                top_keys = tuple(sorted(msg.keys()))
                msg_keys[top_keys] += 1
                channel = msg.get("channel") or "unknown"
                counts[channel] += 1

                # サンプル保持 (channel ごとに先頭2件)
                key = f"{channel}"
                if len(samples[key]) < 2:
                    samples[key].append(msg)

                # sequence number 観察
                data = msg.get("data") or {}
                if isinstance(data, dict):
                    coin = data.get("coin") or msg.get("coin")
                    seq = data.get("seq") or msg.get("seq")
                    if seq is not None and coin is not None:
                        seq_seen[coin].append(int(seq))
        finally:
            pass

    return {
        "counts": dict(counts),
        "msg_keys": {str(k): v for k, v in msg_keys.items()},
        "seq_summary": {
            sym: {
                "count": len(s),
                "min": min(s) if s else None,
                "max": max(s) if s else None,
                "monotonic": all(b > a for a, b in itertools.pairwise(s)) if len(s) > 1 else None,
            }
            for sym, s in seq_seen.items()
        },
        "samples_first2_per_channel": dict(samples),
    }


async def rest_probe(url: str, symbols: list[str]) -> dict[str, object]:
    """REST metaAndAssetCtxs と l2Book を 1 回ずつ叩く."""
    out: dict[str, object] = {}
    async with httpx.AsyncClient(timeout=10.0) as client:
        # metaAndAssetCtxs
        resp = await client.post(url, json={"type": "metaAndAssetCtxs"})
        resp.raise_for_status()
        data = resp.json()
        if isinstance(data, list) and len(data) >= 2:
            meta = data[0]
            ctxs = data[1]
            universe = meta.get("universe", [])
            target = [
                {
                    "name": u.get("name"),
                    "szDecimals": u.get("szDecimals"),
                    "maxLeverage": u.get("maxLeverage"),
                    "isOnlyIsolated": u.get("isOnlyIsolated"),
                }
                for u in universe
                if u.get("name") in symbols
            ]
            target_ctxs: list[dict] = []
            for u, c in zip(universe, ctxs, strict=False):
                if u.get("name") in symbols:
                    target_ctxs.append(
                        {
                            "name": u.get("name"),
                            "ctx_keys": sorted(c.keys()) if isinstance(c, dict) else None,
                            "sample_ctx": c,
                        }
                    )
            out["meta_universe_size"] = len(universe)
            out["target_universe"] = target
            out["target_ctxs"] = target_ctxs
        else:
            out["meta_unexpected_format"] = type(data).__name__

        # l2Book per symbol
        l2_summary: dict[str, dict] = {}
        for sym in symbols:
            try:
                r = await client.post(url, json={"type": "l2Book", "coin": sym})
                r.raise_for_status()
                d = r.json()
                l2_summary[sym] = {
                    "keys": sorted(d.keys()) if isinstance(d, dict) else None,
                    "has_levels": "levels" in d if isinstance(d, dict) else None,
                    "n_bid_levels": len(d["levels"][0])
                    if isinstance(d, dict) and d.get("levels")
                    else None,
                    "n_ask_levels": len(d["levels"][1])
                    if isinstance(d, dict) and len(d.get("levels", [])) >= 2
                    else None,
                    "time_field_present": "time" in d if isinstance(d, dict) else None,
                    "sample_top_bid": d["levels"][0][0]
                    if isinstance(d, dict) and d.get("levels") and d["levels"][0]
                    else None,
                }
            except Exception as exc:
                l2_summary[sym] = {"error": str(exc)}
        out["l2book_per_symbol"] = l2_summary
    return out


async def amain(seconds: int) -> None:
    cfg = load_config(
        [
            REPO_ROOT / "config" / "default.yaml",
            REPO_ROOT / "config" / "local.yaml",
        ]
    )
    setup_logging(cfg.logging)

    print(f"=== HL API dry-run for {seconds}s ===")
    print(f"WS  : {cfg.hyperliquid.ws_url}")
    print(f"REST: {cfg.hyperliquid.info_api_url}")
    print(f"symbols: {cfg.hyperliquid.all_symbols}")

    rest_result = await rest_probe(cfg.hyperliquid.info_api_url, cfg.hyperliquid.all_symbols)
    print("\n=== REST probe ===")
    print(json.dumps(rest_result, indent=2, default=str)[:4000])

    ws_result = await ws_probe(cfg.hyperliquid.ws_url, cfg.hyperliquid.all_symbols, seconds)
    print("\n=== WS probe ===")
    print(json.dumps(ws_result, indent=2, default=str)[:6000])

    # 保存
    out_dir = REPO_ROOT / "data" / "dryrun"
    out_dir.mkdir(parents=True, exist_ok=True)
    fname = f"dryrun-{datetime.now(UTC).strftime('%Y%m%dT%H%M%SZ')}.json"
    out_path = out_dir / fname
    out_path.write_text(
        json.dumps(
            {
                "started_at": datetime.now(UTC).isoformat(),
                "seconds": seconds,
                "rest": rest_result,
                "ws": ws_result,
            },
            indent=2,
            default=str,
        ),
        encoding="utf-8",
    )
    print(f"\nsaved: {out_path}")


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--seconds",
        type=int,
        default=DEFAULT_SECONDS,
        help=f"WS 観測時間 (default: {DEFAULT_SECONDS})",
    )
    args = parser.parse_args()
    asyncio.run(amain(args.seconds))


if __name__ == "__main__":
    main()
