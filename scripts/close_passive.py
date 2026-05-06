"""mainnet close v1: 既存 long/short position を passive_follow で slice 解消。

build_*_v3*.py の close 版。target name (symbol) / slice_size / slice_times を
CLI 引数で受ける。reduce_only=True を強制し、close 中に position が反転する
事故を絶対に防ぐ。

Usage:
    .venv/bin/python scripts/close_passive.py \\
        --symbol HYPE --slice-size 6.5 --slice-times 10 [--interval-s 300]

設計:
- 各 round で `start_exec passive <SYMBOL> close <slice-size>` を投げる
  (post-only ALO maker、long なら sell on best_ask)
- algo は server 内部で max_total_ms 走り、partial も含めて drain
- Python は status を polling して terminal まで待機 (max 90s)
- 完了後 HL info `clearinghouseState` で master の真の size を fetch
- delta 判定:
    delta ≈ -slice_size      → 約定 OK (long の場合 size 減少)
    delta ≈ 0                → no fill (連続 MAX_CONSECUTIVE_NO_FILL で halt)
    delta < -slice_size*1.6  → 二重約定検知 → emergency_stop
- 各 round 後に HL openOrders で orphan check → あれば emergency_stop
- target_size を超えて close しないよう、残量 = min(slice_size, |current|) で
  ratchet (= 最終 round で position が小さくなっても踏み外さない)
"""

from __future__ import annotations
import argparse
import asyncio
import json
import socket
import sys
import time
import urllib.request
import urllib.error
from datetime import datetime, timezone
from decimal import Decimal

sys.path.insert(0, "src")
import websockets  # noqa: F401, E402  (verify import resolves)
from executor.client import ExecutorClient  # noqa: E402

OPERATOR_ID = "sdkfjs"
BASE_URL = "http://127.0.0.1:8085"
MASTER = "0xfe3e32cd4443e395ec0400bf828a34309e517d2d"
HL_INFO = "https://api.hyperliquid.xyz/info"

ROUND_HARD_TIMEOUT_S = 90.0
STATUS_POLL_S = 2.0
HL_INFO_MAX_RETRIES = 3
HL_INFO_RETRY_BACKOFF_S = 1.5
MAX_CONSECUTIVE_NO_FILL = 3


def now() -> str:
    return datetime.now(timezone.utc).strftime("%H:%M:%S")


def hl_info_post(body: dict) -> object:
    last_err: Exception | None = None
    for attempt in range(HL_INFO_MAX_RETRIES):
        try:
            req = urllib.request.Request(
                HL_INFO,
                data=json.dumps(body).encode(),
                headers={"Content-Type": "application/json"},
            )
            return json.loads(urllib.request.urlopen(req, timeout=10).read())
        except (urllib.request.HTTPError, urllib.error.URLError,
                TimeoutError, socket.timeout, OSError) as e:
            last_err = e
            if attempt < HL_INFO_MAX_RETRIES - 1:
                wait = HL_INFO_RETRY_BACKOFF_S * (2 ** attempt)
                print(f"[{now()}] hl_info_post: {type(e).__name__} {e}, retry in {wait}s "
                      f"({attempt+1}/{HL_INFO_MAX_RETRIES})", flush=True)
                time.sleep(wait)
    raise RuntimeError(f"hl_info_post: all {HL_INFO_MAX_RETRIES} retries failed: {last_err!r}")


async def fetch_master_size(symbol: str) -> Decimal:
    snap = await asyncio.to_thread(
        hl_info_post,
        {"type": "clearinghouseState", "user": MASTER},
    )
    for ap in snap.get("assetPositions", []):
        p = ap.get("position", {})
        if p.get("coin") == symbol:
            return Decimal(str(p.get("szi", "0")))
    return Decimal(0)


async def fetch_master_open_orders(symbol: str) -> list[dict]:
    oo = await asyncio.to_thread(
        hl_info_post,
        {"type": "openOrders", "user": MASTER},
    )
    if not isinstance(oo, list):
        return []
    return [o for o in oo if o.get("coin") == symbol]


async def detect_and_clear_orphan_orders(cli: ExecutorClient, symbol: str, idx: int) -> int:
    orphans = await fetch_master_open_orders(symbol)
    if not orphans:
        return 0
    print(f"[{now()}] r{idx}: !!! detected {len(orphans)} orphan {symbol} orders on HL !!!",
          flush=True)
    for o in orphans:
        print(f"[{now()}] r{idx}:   orphan oid={o.get('oid')} side={o.get('side')} "
              f"sz={o.get('sz')} px={o.get('limitPx')}", flush=True)
    print(f"[{now()}] r{idx}: firing emergency_stop to clear orphans", flush=True)
    try:
        stop = await cli.emergency_stop()
        print(f"[{now()}] r{idx}: emergency_stop result: {stop}", flush=True)
    except Exception as e:  # noqa: BLE001
        print(f"[{now()}] r{idx}: emergency_stop err: {e!r}", flush=True)
    await asyncio.sleep(5.0)
    still = await fetch_master_open_orders(symbol)
    if still:
        print(f"[{now()}] r{idx}: !!! {len(still)} orphans STILL present !!!", flush=True)
        for o in still:
            print(f"[{now()}] r{idx}:   stuck oid={o.get('oid')} sz={o.get('sz')} "
                  f"px={o.get('limitPx')}", flush=True)
        raise RuntimeError(
            f"r{idx}: {len(still)} orphan orders cannot be cleared by emergency_stop"
        )
    print(f"[{now()}] r{idx}: orphans cleared successfully ({len(orphans)} orders)", flush=True)
    return len(orphans)


async def wait_exec_done_via_status(cli: ExecutorClient, exec_id: str, idx: int) -> dict:
    final = {"done": False, "aborted": False, "abort_reason": None,
             "filled_size": Decimal("0"), "n_fills": 0}
    deadline = asyncio.get_running_loop().time() + ROUND_HARD_TIMEOUT_S
    last_status = None
    while asyncio.get_running_loop().time() < deadline:
        try:
            st = await cli.status(exec_id)
        except Exception as e:  # noqa: BLE001
            print(f"[{now()}] r{idx}: status fetch err {e!r}", flush=True)
            await asyncio.sleep(STATUS_POLL_S)
            continue
        status_str = st.get("status")
        if status_str != last_status:
            print(f"[{now()}] r{idx}: status='{status_str}' "
                  f"report={'<set>' if st.get('report') else 'null'}", flush=True)
            last_status = status_str
        if status_str in ("completed", "aborted", "failed"):
            report = st.get("report") or {}
            final["done"] = (status_str == "completed")
            final["aborted"] = bool(report.get("aborted")) or (status_str == "aborted")
            final["abort_reason"] = report.get("abort_reason") or st.get("error")
            final["filled_size"] = Decimal(str(report.get("filled_size", "0")))
            final["n_fills"] = len(report.get("fills", []))
            # Issue #80 推奨: aborted/failed 時に abort_reason をログに残す
            if final["aborted"] or status_str in ("aborted", "failed"):
                print(f"[{now()}] r{idx}: terminal status={status_str} "
                      f"abort_reason={final['abort_reason']!r} "
                      f"filled={final['filled_size']} n_fills={final['n_fills']}",
                      flush=True)
            return final
        await asyncio.sleep(STATUS_POLL_S)
    print(f"[{now()}] r{idx}: status-poll deadline reached", flush=True)
    return final


async def assert_prev_exec_terminal(cli: ExecutorClient, prev_exec_id: str | None,
                                     max_wait_s: float = 20.0) -> None:
    if prev_exec_id is None:
        return
    deadline = asyncio.get_running_loop().time() + max_wait_s
    while asyncio.get_running_loop().time() < deadline:
        try:
            st = await cli.status(prev_exec_id)
        except Exception as e:  # noqa: BLE001
            print(f"[{now()}] prev exec status check err {e!r}", flush=True)
            await asyncio.sleep(1.0)
            continue
        status_str = st.get("status")
        if status_str in ("completed", "aborted", "failed"):
            return
        print(f"[{now()}] waiting prev exec {prev_exec_id} terminal "
              f"(currently '{status_str}')", flush=True)
        await asyncio.sleep(1.0)
    raise RuntimeError(f"prev exec {prev_exec_id} not terminal after {max_wait_s}s")


async def run_one_round(cli: ExecutorClient, args: argparse.Namespace,
                          idx: int, prev_exec_id: str | None,
                          slice_size_for_round: Decimal) -> dict:
    symbol = args.symbol
    print(f"[{now()}] === round {idx}/{args.slice_times} === close size={slice_size_for_round} {symbol}",
          flush=True)
    await assert_prev_exec_terminal(cli, prev_exec_id)
    pre_hl = await fetch_master_size(symbol)
    print(f"[{now()}] r{idx}: pre HL {symbol} size = {pre_hl}", flush=True)

    if pre_hl == 0:
        # 既に position 無し → 以降の round は無意味。run_close で外側 break 判断する。
        return {
            "exec_id": None, "pre_hl": pre_hl, "post_hl": pre_hl, "delta": Decimal(0),
            "done": True, "aborted": False, "abort_reason": None,
            "filled_size": Decimal(0), "n_fills": 0, "orphans_cleared": 0,
            "no_position": True,
        }

    # close は target_size を絶対量で渡す。HL 側で min(target, |current|) に
    # 自動 clip されるが、念のため Python 側でも clip しておく。
    target_for_round = slice_size_for_round.copy_abs()
    if target_for_round > pre_hl.copy_abs():
        target_for_round = pre_hl.copy_abs()

    resp = await cli.start(
        "passive", symbol, "close", str(target_for_round),
        params={
            "max_total_ms": args.max_total_ms,
            "max_book_age_ms": 3000,
            "repost_threshold_ticks": 1,
            # 反転防止のため reduce_only を必ず立てる
            "reduce_only": True,
        },
    )
    exec_id = resp.exec_id
    print(f"[{now()}] r{idx}: exec_id={exec_id} (close, reduce_only=True)", flush=True)

    final = await wait_exec_done_via_status(cli, exec_id, idx)
    if not (final["done"] or final["aborted"]):
        print(f"[{now()}] r{idx}: status not terminal, sending cancel", flush=True)
        try:
            await cli.cancel(exec_id)
        except Exception as e:  # noqa: BLE001
            print(f"[{now()}] r{idx}: cancel err {e!r}", flush=True)
        await asyncio.sleep(3.0)

    await asyncio.sleep(3.0)
    orphans_cleared = await detect_and_clear_orphan_orders(cli, symbol, idx)

    post_hl = await fetch_master_size(symbol)
    delta = post_hl - pre_hl  # close なので long の場合 delta ≤ 0
    print(f"[{now()}] r{idx}: post HL {symbol} size = {post_hl} delta = {delta} "
          f"orphans_cleared={orphans_cleared}", flush=True)

    final["pre_hl"] = pre_hl
    final["post_hl"] = post_hl
    final["delta"] = delta
    final["exec_id"] = exec_id
    final["orphans_cleared"] = orphans_cleared
    final["no_position"] = False
    return final


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="passive_follow による既存 position の slice 解消 (mainnet close)",
    )
    p.add_argument("--symbol", required=True, help="HL perp symbol (例: HYPE, ETH)")
    p.add_argument("--slice-size", required=True, type=Decimal,
                   help="1 round あたりの close 量 (例: 6.5)")
    p.add_argument("--slice-times", required=True, type=int,
                   help="round 数 (例: 10)")
    p.add_argument("--interval-s", type=float, default=300.0,
                   help="round 間 sleep 秒 (default 300=5min)")
    p.add_argument("--max-total-ms", type=int, default=60000,
                   help="algo の round 内 max_total_ms (default 60000)")
    p.add_argument("--operator-id", default=OPERATOR_ID)
    p.add_argument("--base-url", default=BASE_URL)
    return p.parse_args(argv)


async def run_close(args: argparse.Namespace) -> None:
    symbol = args.symbol
    slice_size: Decimal = args.slice_size.copy_abs()
    rounds = args.slice_times

    delta_low = slice_size * Decimal("0.90")
    delta_high = slice_size * Decimal("1.10")
    delta_danger = slice_size * Decimal("1.60")

    print(f"[{now()}] passive_follow CLOSE (PR-D10 race-cap, reduce_only=True): "
          f"{rounds} rounds × {slice_size} {symbol}, interval {args.interval_s:.0f}s, "
          f"op={args.operator_id}", flush=True)
    print(f"[{now()}] tolerances: low={delta_low} high={delta_high} danger={delta_danger}",
          flush=True)
    print(f"[{now()}] base_url={args.base_url} master={MASTER}", flush=True)

    async with ExecutorClient(args.base_url, operator_id=args.operator_id) as cli:
        h = await cli.health()
        print(f"[{now()}] server health: ws_connected={h['health']['ws_connected']} "
              f"running={h.get('running_executions', 0)} "
              f"ws_msgs={h['health']['ws_message_count']}", flush=True)
        sz0 = await fetch_master_size(symbol)
        if sz0 == 0:
            print(f"[{now()}] no {symbol} position to close. exit.", flush=True)
            return
        side_str = "LONG" if sz0 > 0 else "SHORT"
        target_after = sz0 - (sz0.copy_sign(slice_size) if False else
                              # close direction = -sign(sz0), so signed delta per round = -sign(sz0)*slice_size
                              (slice_size if sz0 > 0 else -slice_size)) * rounds
        print(f"[{now()}] start HL {symbol} = {sz0} ({side_str}) | "
              f"plan close = {slice_size * rounds} | target after = {target_after}",
              flush=True)

        results: list[dict] = []
        consecutive_no_fill = 0
        prev_exec_id: str | None = None
        try:
            for i in range(1, rounds + 1):
                round_started_at = asyncio.get_running_loop().time()
                # ratchet: 残量を毎 round 真値で取り直し、min(slice_size, |current|)
                cur = await fetch_master_size(symbol)
                if cur == 0:
                    print(f"[{now()}] r{i}: position already 0, stopping", flush=True)
                    break
                this_round_size = slice_size.min(cur.copy_abs())
                r = await run_one_round(cli, args, i, prev_exec_id, this_round_size)
                results.append(r)
                if r.get("no_position"):
                    print(f"[{now()}] r{i}: position vanished mid-round, stopping", flush=True)
                    break
                prev_exec_id = r["exec_id"]
                # close では delta は long の場合 ≤ 0、short の場合 ≥ 0。絶対値で判定。
                delta = r["delta"]
                abs_delta = delta.copy_abs()
                # 方向の整合性: long → delta が負、short → delta が正 が期待。
                # 逆方向なら異常 (reduce_only=True なので起き得ないはずだが念のため)
                if (sz0 > 0 and delta > 0) or (sz0 < 0 and delta < 0):
                    print(f"[{now()}] r{i}: WRONG-DIRECTION delta={delta} (sz0={sz0}), "
                          f"firing emergency_stop", flush=True)
                    try:
                        stop = await cli.emergency_stop()
                        print(f"[{now()}] emergency_stop: {stop}", flush=True)
                    except Exception as e:  # noqa: BLE001
                        print(f"[{now()}] emergency_stop err: {e!r}", flush=True)
                    raise RuntimeError(
                        f"r{i}: position grew on close (delta {delta}, sz0 {sz0})"
                    )

                if abs_delta > delta_danger:
                    print(f"[{now()}] r{i}: ABNORMAL |delta|={abs_delta} > {delta_danger}, "
                          f"firing emergency_stop", flush=True)
                    try:
                        stop = await cli.emergency_stop()
                        print(f"[{now()}] emergency_stop: {stop}", flush=True)
                    except Exception as e:  # noqa: BLE001
                        print(f"[{now()}] emergency_stop err: {e!r}", flush=True)
                    raise RuntimeError(
                        f"r{i} |delta| {abs_delta} > {delta_danger}, halting"
                    )

                if r["orphans_cleared"] > 0:
                    raise RuntimeError(
                        f"r{i}: orphan clear fired emergency_stop "
                        f"({r['orphans_cleared']} orders) — server is shutting down"
                    )

                if delta_low <= abs_delta <= delta_high:
                    consecutive_no_fill = 0
                    print(f"[{now()}] r{i}: ✓ closed normally (delta={delta})", flush=True)
                elif abs_delta == 0:
                    consecutive_no_fill += 1
                    print(f"[{now()}] r{i}: ✗ no fill "
                          f"(consecutive={consecutive_no_fill}/{MAX_CONSECUTIVE_NO_FILL})",
                          flush=True)
                    if consecutive_no_fill >= MAX_CONSECUTIVE_NO_FILL:
                        print(f"[{now()}] {MAX_CONSECUTIVE_NO_FILL} consecutive no-fill, halting",
                              flush=True)
                        break
                else:
                    consecutive_no_fill = 0
                    print(f"[{now()}] r{i}: partial delta={delta}, continuing", flush=True)

                if r["post_hl"] == 0:
                    print(f"[{now()}] r{i}: position closed to 0, stopping", flush=True)
                    break

                if i < rounds:
                    elapsed = asyncio.get_running_loop().time() - round_started_at
                    sleep_s = max(0.0, args.interval_s - elapsed)
                    print(f"[{now()}] r{i}: round took {elapsed:.1f}s, sleeping "
                          f"{sleep_s:.0f}s until next round", flush=True)
                    await asyncio.sleep(sleep_s)
        except KeyboardInterrupt:
            print(f"[{now()}] KeyboardInterrupt", flush=True)
            raise

        post_hl = await fetch_master_size(symbol)
        total_closed = sz0 - post_hl
        print(f"[{now()}] final HL {symbol} = {post_hl} | total closed = {total_closed} "
              f"| started at {sz0}", flush=True)


def main() -> None:
    args = parse_args()
    if args.slice_size <= 0:
        print(f"--slice-size must be > 0, got {args.slice_size}", file=sys.stderr)
        sys.exit(2)
    if args.slice_times <= 0:
        print(f"--slice-times must be > 0, got {args.slice_times}", file=sys.stderr)
        sys.exit(2)
    try:
        asyncio.run(run_close(args))
    except KeyboardInterrupt:
        print(f"[{now()}] interrupted by user", flush=True)


if __name__ == "__main__":
    main()
