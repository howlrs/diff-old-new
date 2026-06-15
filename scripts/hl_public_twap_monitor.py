#!/usr/bin/env python3
"""Monitor public, coin-level Hyperliquid TWAP activity.

This script is for HyperCore data providers that expose the all-user TWAP
stream over JSON-RPC/WebSocket, for example endpoints compatible with
``hl_subscribe`` and ``hl_getLatestBlocks``. The official Hyperliquid
``userTwap*`` endpoints are user-scoped and cannot answer "all users for coin".

Examples:
    python scripts/hl_public_twap_monitor.py \
        --coin HYPE \
        --ws-url wss://YOUR-ENDPOINT/ws \
        --http-url https://YOUR-ENDPOINT/hypercore \
        --seed-blocks 500 \
        --seconds 300

    python scripts/hl_public_twap_monitor.py \
        --coin HYPE \
        --http-url https://YOUR-ENDPOINT/hypercore \
        --compare-history \
        --window-s 3600 \
        --compare-offset-s 43200

    python scripts/hl_public_twap_monitor.py \
        --coin HYPE \
        --user-sample \
        --discover-users recent-trades \
        --sample-rank-by target_volume \
        --sample-top-n 100

The output has two different meanings:
- Planned TWAP activity comes from TWAP order state events.
- Actual TWAP execution comes from trade/fill events with ``twapId``.
"""

from __future__ import annotations

import argparse
import asyncio
import csv
import json
import os
import re
import signal
import sys
from collections import defaultdict, deque
from contextlib import suppress
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from decimal import Decimal, InvalidOperation
from typing import Any

import httpx
import websockets

HL_NATIVE_TWAP_INTERVAL_S = Decimal("30")
DEFAULT_BLOCKS_PER_SECOND = Decimal("12")
HL_INFO_URL = "https://api.hyperliquid.xyz/info"
ADDRESS_RE = re.compile(r"^0x[a-fA-F0-9]{40}$")


def utc_now_ms() -> int:
    return int(datetime.now(UTC).timestamp() * 1000)


def iso_from_ms(ms: int | None) -> str:
    if ms is None:
        return "-"
    return datetime.fromtimestamp(ms / 1000, UTC).isoformat(timespec="seconds")


def dec(value: Any, default: str = "0") -> Decimal:
    try:
        return Decimal(str(default if value is None else value))
    except (InvalidOperation, ValueError):
        return Decimal(default)


def side_label(side: str | None) -> str:
    if side == "B":
        return "BUY / build long or close short"
    if side == "A":
        return "SELL / build short or close long"
    return side or "unknown"


def status_label(status: Any) -> str:
    if isinstance(status, dict):
        return str(status.get("status") or status.get("type") or "unknown")
    return str(status or "unknown")


def event_time_ms(event: dict[str, Any]) -> int | None:
    value = event.get("time")
    if isinstance(value, int | float):
        return int(value)
    if isinstance(value, str):
        try:
            return int(datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp() * 1000)
        except ValueError:
            return None
    state = event.get("state")
    if isinstance(state, dict) and isinstance(state.get("timestamp"), int | float):
        return int(state["timestamp"])
    return None


def get_twap_id(event: dict[str, Any]) -> int | str | None:
    return event.get("twap_id") or event.get("twapId") or event.get("id")


def normalize_user_address(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    addr = value.strip()
    return addr.lower() if ADDRESS_RE.match(addr) else None


def normalize_stream_events(msg: dict[str, Any]) -> list[Any]:
    """Extract event arrays from common HyperCore stream response shapes."""
    data = msg.get("data")
    if isinstance(data, dict):
        events = data.get("events")
        if isinstance(events, list):
            return events
    events = msg.get("events")
    if isinstance(events, list):
        return events
    result = msg.get("result")
    if isinstance(result, dict):
        events = result.get("events")
        if isinstance(events, list):
            return events
        blocks = result.get("blocks")
        if isinstance(blocks, list):
            out: list[Any] = []
            for block in blocks:
                if isinstance(block, dict) and isinstance(block.get("events"), list):
                    out.extend(block["events"])
            return out
    return []


def normalize_blocks(msg: dict[str, Any]) -> list[dict[str, Any]]:
    result = msg.get("result")
    if isinstance(result, dict):
        blocks = result.get("blocks")
        if isinstance(blocks, list):
            return [block for block in blocks if isinstance(block, dict)]
    blocks = msg.get("blocks")
    if isinstance(blocks, list):
        return [block for block in blocks if isinstance(block, dict)]
    return []


@dataclass
class TwapOrder:
    twap_id: str
    coin: str
    side: str
    total_size: Decimal
    executed_size: Decimal
    executed_notional: Decimal
    minutes: Decimal
    reduce_only: bool
    randomize: bool
    created_ms: int | None
    last_event_ms: int | None
    status: str

    @property
    def end_ms(self) -> int | None:
        if self.created_ms is None:
            return None
        return self.created_ms + int(self.minutes * Decimal(60_000))

    @property
    def remaining_size(self) -> Decimal:
        return max(Decimal("0"), self.total_size - self.executed_size)

    @property
    def planned_slices(self) -> Decimal:
        # Native TWAP suborders are normally 30 seconds apart. If randomize=true,
        # treat this as an expected cadence, not an exact clock.
        return max(Decimal("1"), self.minutes * Decimal("2"))

    @property
    def planned_slice_size(self) -> Decimal:
        return self.total_size / self.planned_slices

    @property
    def planned_size_per_min(self) -> Decimal:
        if self.minutes <= 0:
            return Decimal("0")
        return self.total_size / self.minutes

    def is_active(self, now_ms: int) -> bool:
        if self.status not in {"activated", "running", "active"}:
            return False
        end = self.end_ms
        return end is None or now_ms <= end


@dataclass
class FillAgg:
    count: int = 0
    total_size: Decimal = Decimal("0")
    total_notional: Decimal = Decimal("0")
    first_ms: int | None = None
    last_ms: int | None = None

    def add(self, ts_ms: int, size: Decimal, price: Decimal) -> None:
        self.count += 1
        self.total_size += size
        self.total_notional += size * price
        self.first_ms = ts_ms if self.first_ms is None else min(self.first_ms, ts_ms)
        self.last_ms = ts_ms if self.last_ms is None else max(self.last_ms, ts_ms)

    @property
    def avg_interval_s(self) -> Decimal | None:
        if self.count < 2 or self.first_ms is None or self.last_ms is None:
            return None
        return Decimal(self.last_ms - self.first_ms) / Decimal(1000) / Decimal(self.count - 1)

    @property
    def vwap(self) -> Decimal | None:
        if self.total_size == 0:
            return None
        return self.total_notional / self.total_size


class TwapMonitor:
    def __init__(self, coin: str) -> None:
        self.coin = coin
        self.orders: dict[str, TwapOrder] = {}
        self.fills: dict[str, FillAgg] = defaultdict(FillAgg)
        self.recent_fills: deque[tuple[int, str, str, Decimal, Decimal]] = deque(maxlen=20_000)

    def ingest_twap_event(self, event: dict[str, Any]) -> None:
        state = event.get("state")
        if not isinstance(state, dict):
            return
        coin = str(state.get("coin") or "")
        if coin.upper() != self.coin.upper():
            return
        twap_id = get_twap_id(event)
        if twap_id is None:
            return
        created_ms = int(state["timestamp"]) if isinstance(state.get("timestamp"), int | float) else None
        order = TwapOrder(
            twap_id=str(twap_id),
            coin=coin,
            side=str(state.get("side") or ""),
            total_size=dec(state.get("sz")),
            executed_size=dec(state.get("executedSz")),
            executed_notional=dec(state.get("executedNtl")),
            minutes=dec(state.get("minutes"), "0"),
            reduce_only=bool(state.get("reduceOnly")),
            randomize=bool(state.get("randomize")),
            created_ms=created_ms,
            last_event_ms=event_time_ms(event),
            status=status_label(event.get("status")),
        )
        self.orders[order.twap_id] = order

    def ingest_trade_event(self, event: Any) -> None:
        user: str | None = None
        fill: Any = event
        if isinstance(event, list | tuple) and len(event) >= 2:
            user = str(event[0])
            fill = event[1]
        if not isinstance(fill, dict):
            return
        coin = str(fill.get("coin") or "")
        if coin.upper() != self.coin.upper():
            return
        twap_id = fill.get("twapId") or fill.get("twap_id")
        if twap_id is None:
            return
        ts_ms = fill.get("time")
        if not isinstance(ts_ms, int | float):
            return
        size = dec(fill.get("sz"))
        price = dec(fill.get("px"))
        side = str(fill.get("side") or "")
        tid = str(twap_id)
        self.fills[tid].add(int(ts_ms), size, price)
        self.recent_fills.append((int(ts_ms), tid, side, size, price))
        if user and tid not in self.orders:
            # Keep a placeholder so fills are still visible even if activation was
            # outside the seeded/live TWAP state window.
            self.orders[tid] = TwapOrder(
                twap_id=tid,
                coin=coin,
                side=side,
                total_size=Decimal("0"),
                executed_size=Decimal("0"),
                executed_notional=Decimal("0"),
                minutes=Decimal("0"),
                reduce_only=False,
                randomize=False,
                created_ms=None,
                last_event_ms=int(ts_ms),
                status="fills_observed_only",
            )

    def window_fill_summary(self, window_s: int) -> dict[str, dict[str, Decimal | int]]:
        cutoff = utc_now_ms() - window_s * 1000
        out: dict[str, dict[str, Decimal | int]] = {}
        for ts_ms, _twap_id, side, size, price in self.recent_fills:
            if ts_ms < cutoff:
                continue
            key = side or "unknown"
            row = out.setdefault(
                key,
                {
                    "fills": 0,
                    "size": Decimal("0"),
                    "notional": Decimal("0"),
                },
            )
            row["fills"] = int(row["fills"]) + 1
            row["size"] = row["size"] + size  # type: ignore[operator]
            row["notional"] = row["notional"] + size * price  # type: ignore[operator]
        return out

    def snapshot(self, window_s: int) -> dict[str, Any]:
        now_ms = utc_now_ms()
        active = [order for order in self.orders.values() if order.is_active(now_ms)]
        active.sort(key=lambda row: row.created_ms or 0)
        planned_by_side: dict[str, dict[str, Decimal | int]] = {}
        for order in active:
            key = order.side or "unknown"
            row = planned_by_side.setdefault(
                key,
                {
                    "orders": 0,
                    "remaining_size": Decimal("0"),
                    "planned_size_per_min": Decimal("0"),
                    "planned_slice_size_sum": Decimal("0"),
                },
            )
            row["orders"] = int(row["orders"]) + 1
            row["remaining_size"] = row["remaining_size"] + order.remaining_size  # type: ignore[operator]
            row["planned_size_per_min"] = (  # type: ignore[operator]
                row["planned_size_per_min"] + order.planned_size_per_min
            )
            row["planned_slice_size_sum"] = (  # type: ignore[operator]
                row["planned_slice_size_sum"] + order.planned_slice_size
            )

        return {
            "coin": self.coin,
            "now": iso_from_ms(now_ms),
            "active_orders": [order_to_output(order, self.fills.get(order.twap_id)) for order in active],
            "planned_by_side": planned_by_side,
            f"actual_fills_last_{window_s}s_by_side": self.window_fill_summary(window_s),
            "known_orders": len(self.orders),
            "known_twap_ids_with_fills": len(self.fills),
        }


@dataclass
class WindowAgg:
    fills: int = 0
    twap_ids: set[str] | None = None
    size: Decimal = Decimal("0")
    notional: Decimal = Decimal("0")
    first_ms: int | None = None
    last_ms: int | None = None

    def __post_init__(self) -> None:
        if self.twap_ids is None:
            self.twap_ids = set()

    def add(self, ts_ms: int, twap_id: str, size: Decimal, price: Decimal) -> None:
        self.fills += 1
        if twap_id:
            self.twap_ids.add(twap_id)
        self.size += size
        self.notional += size * price
        self.first_ms = ts_ms if self.first_ms is None else min(self.first_ms, ts_ms)
        self.last_ms = ts_ms if self.last_ms is None else max(self.last_ms, ts_ms)

    @property
    def twap_count(self) -> int:
        return len(self.twap_ids or ())

    @property
    def avg_interval_s(self) -> Decimal | None:
        if self.fills < 2 or self.first_ms is None or self.last_ms is None:
            return None
        return Decimal(self.last_ms - self.first_ms) / Decimal(1000) / Decimal(self.fills - 1)

    @property
    def vwap(self) -> Decimal | None:
        if self.size == 0:
            return None
        return self.notional / self.size

    def to_output(self) -> dict[str, Any]:
        return {
            "fills": self.fills,
            "twaps": self.twap_count,
            "size": self.size,
            "notional": self.notional,
            "avg_interval_s": self.avg_interval_s,
            "vwap": self.vwap,
            "first": iso_from_ms(self.first_ms),
            "last": iso_from_ms(self.last_ms),
        }


def empty_side_aggs() -> dict[str, WindowAgg]:
    return {"B": WindowAgg(), "A": WindowAgg(), "unknown": WindowAgg()}


def window_ratio(now_value: Decimal, past_value: Decimal) -> Decimal | None:
    if past_value == 0:
        return None
    return now_value / past_value


def window_share(part_value: Decimal, total_value: Decimal) -> Decimal | None:
    if total_value == 0:
        return None
    return part_value / total_value


def twap_notional_imbalance(side_aggs: dict[str, WindowAgg]) -> Decimal | None:
    buy = side_aggs["B"].notional
    sell = side_aggs["A"].notional
    total = buy + sell
    if total == 0:
        return None
    return (buy - sell) / total


def ingest_historical_trade(
    event: Any,
    coin: str,
    windows: dict[str, tuple[int, int]],
    aggs: dict[str, dict[str, WindowAgg]],
) -> None:
    fill: Any = event
    if isinstance(event, list | tuple) and len(event) >= 2:
        fill = event[1]
    if not isinstance(fill, dict):
        return
    if str(fill.get("coin") or "").upper() != coin.upper():
        return
    twap_id = fill.get("twapId") or fill.get("twap_id")
    if twap_id is None:
        return
    ts_ms = fill.get("time")
    if not isinstance(ts_ms, int | float):
        return
    ts = int(ts_ms)
    side = str(fill.get("side") or "unknown")
    if side not in {"B", "A"}:
        side = "unknown"
    for name, (start_ms, end_ms) in windows.items():
        if start_ms <= ts < end_ms:
            aggs[name][side].add(ts, str(twap_id), dec(fill.get("sz")), dec(fill.get("px")))


@dataclass
class UserRankStats:
    user: str
    input_rank: int
    target_volume: Decimal = Decimal("0")
    target_fills: int = 0
    all_volume: Decimal = Decimal("0")
    all_fills: int = 0
    account_value: Decimal = Decimal("0")
    user_fills_seen: int = 0
    twap_fills_seen: int = 0

    def score(self, rank_by: str) -> Decimal:
        if rank_by == "target_volume":
            return self.target_volume
        if rank_by == "target_fills":
            return Decimal(self.target_fills)
        if rank_by == "all_volume":
            return self.all_volume
        if rank_by == "all_fills":
            return Decimal(self.all_fills)
        if rank_by == "account_value":
            return self.account_value
        if rank_by == "input_order":
            return Decimal(-self.input_rank)
        raise ValueError(f"unknown rank_by: {rank_by}")

    def to_output(self, rank_by: str) -> dict[str, Any]:
        return {
            "user": self.user,
            "score": self.score(rank_by),
            "target_volume": self.target_volume,
            "target_fills": self.target_fills,
            "all_volume": self.all_volume,
            "all_fills": self.all_fills,
            "account_value": self.account_value,
            "user_fills_seen": self.user_fills_seen,
            "twap_fills_seen": self.twap_fills_seen,
        }


def read_user_addresses(path: str) -> list[str]:
    seen: set[str] = set()
    users: list[str] = []
    with open(path, encoding="utf-8", newline="") as fh:
        sample = fh.read(4096)
        fh.seek(0)
        if path.endswith(".json"):
            raw = json.load(fh)
            values: list[Any]
            if isinstance(raw, list):
                values = raw
            elif isinstance(raw, dict):
                values = list(raw.get("users") or raw.get("addresses") or raw.values())
            else:
                values = []
            for item in values:
                if isinstance(item, dict):
                    item = item.get("user") or item.get("address") or item.get("account")
                addr = normalize_user_address(item)
                if addr and addr not in seen:
                    seen.add(addr)
                    users.append(addr)
            return users

        if "," in sample or "\t" in sample:
            dialect = csv.Sniffer().sniff(sample, delimiters=",\t")
            reader = csv.DictReader(fh, dialect=dialect)
            fieldnames = reader.fieldnames or []
            address_field = next(
                (
                    name
                    for name in fieldnames
                    if name.lower() in {"user", "address", "account", "wallet"}
                ),
                fieldnames[0] if fieldnames else "",
            )
            for row in reader:
                addr = normalize_user_address(row.get(address_field))
                if addr and addr not in seen:
                    seen.add(addr)
                    users.append(addr)
            return users

        for line in fh:
            addr = normalize_user_address(line.split("#", 1)[0])
            if addr and addr not in seen:
                seen.add(addr)
                users.append(addr)
    return users


async def post_info(client: httpx.AsyncClient, info_url: str, payload: dict[str, Any]) -> Any:
    for attempt in range(6):
        resp = await client.post(info_url, json=payload)
        if resp.status_code == 429 or resp.status_code >= 500:
            retry_after = resp.headers.get("retry-after")
            if retry_after and retry_after.isdigit():
                wait_s = float(retry_after)
            else:
                wait_s = min(2.0 * (attempt + 1), 10.0)
            await asyncio.sleep(wait_s)
            continue
        resp.raise_for_status()
        break
    else:
        resp.raise_for_status()
    data = resp.json()
    if isinstance(data, dict) and data.get("error"):
        raise RuntimeError(f"info endpoint error for {payload.get('type')}: {data['error']}")
    return data


def extract_fill(obj: Any) -> dict[str, Any] | None:
    if not isinstance(obj, dict):
        return None
    fill = obj.get("fill")
    if isinstance(fill, dict):
        merged = dict(fill)
        for key in ("twapId", "twap_id", "time"):
            if key in obj and merged.get(key) is None:
                merged[key] = obj[key]
        return merged
    return obj


def fill_time_ms(fill: dict[str, Any]) -> int | None:
    value = fill.get("time") or fill.get("timestamp")
    if isinstance(value, int | float):
        return int(value)
    return None


def fill_coin(fill: dict[str, Any]) -> str:
    return str(fill.get("coin") or fill.get("coinName") or "")


def fill_side(fill: dict[str, Any]) -> str:
    side = str(fill.get("side") or "unknown")
    return side if side in {"B", "A"} else "unknown"


def fill_notional(fill: dict[str, Any]) -> Decimal:
    notional = dec(fill.get("ntl") or fill.get("notional"))
    if notional:
        return notional.copy_abs()
    return (dec(fill.get("sz")) * dec(fill.get("px"))).copy_abs()


def fill_size(fill: dict[str, Any]) -> Decimal:
    return dec(fill.get("sz")).copy_abs()


def fill_price(fill: dict[str, Any]) -> Decimal:
    return dec(fill.get("px"))


def twap_id_from_fill(fill: dict[str, Any]) -> str:
    return str(fill.get("twapId") or fill.get("twap_id") or "")


def rank_stats_from_user_fills(
    user: str,
    input_rank: int,
    fills: list[Any],
    coin: str,
    ranking_window: tuple[int, int],
) -> UserRankStats:
    stats = UserRankStats(user=user, input_rank=input_rank)
    start_ms, end_ms = ranking_window
    for raw in fills:
        fill = extract_fill(raw)
        if fill is None:
            continue
        ts = fill_time_ms(fill)
        if ts is None or not (start_ms <= ts < end_ms):
            continue
        notional = fill_notional(fill)
        stats.all_fills += 1
        stats.all_volume += notional
        if fill_coin(fill).upper() == coin.upper():
            stats.target_fills += 1
            stats.target_volume += notional
    stats.user_fills_seen = len(fills)
    return stats


def ingest_user_twap_fill(
    raw: Any,
    coin: str,
    windows: dict[str, tuple[int, int]],
    aggs: dict[str, dict[str, WindowAgg]],
    user_aggs: dict[str, dict[str, dict[str, WindowAgg]]],
    user: str,
) -> bool:
    fill = extract_fill(raw)
    if fill is None:
        return False
    if fill_coin(fill).upper() != coin.upper():
        return False
    ts = fill_time_ms(fill)
    if ts is None:
        return False
    side = fill_side(fill)
    size = fill_size(fill)
    price = fill_price(fill)
    twap_id = twap_id_from_fill(fill)
    matched = False
    for window_name, (start_ms, end_ms) in windows.items():
        if start_ms <= ts < end_ms:
            aggs[window_name][side].add(ts, twap_id, size, price)
            user_aggs[user][window_name][side].add(ts, twap_id, size, price)
            matched = True
    return matched


def ingest_user_total_fill(
    raw: Any,
    coin: str,
    windows: dict[str, tuple[int, int]],
    aggs: dict[str, dict[str, WindowAgg]],
) -> bool:
    fill = extract_fill(raw)
    if fill is None:
        return False
    if fill_coin(fill).upper() != coin.upper():
        return False
    ts = fill_time_ms(fill)
    if ts is None:
        return False
    side = fill_side(fill)
    size = fill_size(fill)
    price = fill_price(fill)
    matched = False
    for window_name, (start_ms, end_ms) in windows.items():
        if start_ms <= ts < end_ms:
            aggs[window_name][side].add(ts, "", size, price)
            matched = True
    return matched


def account_value_from_clearinghouse_state(state: Any) -> Decimal:
    if not isinstance(state, dict):
        return Decimal("0")
    margin_summary = state.get("marginSummary")
    if isinstance(margin_summary, dict):
        account_value = dec(margin_summary.get("accountValue"))
        if account_value:
            return account_value
    cross_summary = state.get("crossMarginSummary")
    if isinstance(cross_summary, dict):
        return dec(cross_summary.get("accountValue"))
    return Decimal("0")


async def fetch_user_sample_record(
    client: httpx.AsyncClient,
    info_url: str,
    user: str,
    input_rank: int,
    coin: str,
    ranking_window: tuple[int, int],
    need_account_value: bool,
) -> tuple[UserRankStats, list[Any]]:
    fills_data = await post_info(client, info_url, {"type": "userFills", "user": user})
    fills = fills_data if isinstance(fills_data, list) else []
    stats = rank_stats_from_user_fills(user, input_rank, fills, coin, ranking_window)

    if need_account_value:
        state = await post_info(client, info_url, {"type": "clearinghouseState", "user": user})
        stats.account_value = account_value_from_clearinghouse_state(state)

    twap_data = await post_info(client, info_url, {"type": "userTwapSliceFills", "user": user})
    twap_fills = twap_data if isinstance(twap_data, list) else []
    stats.twap_fills_seen = len(twap_fills)
    return stats, twap_fills


async def discover_recent_trade_users(
    info_url: str,
    coin: str,
    seconds: int,
    poll_s: float,
    max_trades: int,
) -> list[UserRankStats]:
    """Discover candidate users from the free official recentTrades endpoint.

    recentTrades is coin-scoped and returns both counterparties in ``users``.
    This is not a full historical scan; it is a free, observation-window based
    candidate generator.
    """
    stats: dict[str, UserRankStats] = {}
    seen_tids: set[str] = set()
    stop_at = asyncio.get_running_loop().time() + seconds if seconds > 0 else None
    input_rank = 0

    async with httpx.AsyncClient(timeout=10.0) as client:
        while True:
            data = await post_info(client, info_url, {"type": "recentTrades", "coin": coin})
            trades = data if isinstance(data, list) else []
            for raw in trades:
                if not isinstance(raw, dict):
                    continue
                tid = str(raw.get("tid") or raw.get("hash") or "")
                if tid and tid in seen_tids:
                    continue
                if tid:
                    seen_tids.add(tid)
                notional = fill_notional(raw)
                users = raw.get("users")
                if not isinstance(users, list):
                    continue
                for item in users:
                    user = normalize_user_address(item)
                    if user is None:
                        continue
                    if user not in stats:
                        stats[user] = UserRankStats(user=user, input_rank=input_rank)
                        input_rank += 1
                    stats[user].target_fills += 1
                    stats[user].target_volume += notional
                    stats[user].all_fills += 1
                    stats[user].all_volume += notional
                if max_trades > 0 and len(seen_tids) >= max_trades:
                    break
            if max_trades > 0 and len(seen_tids) >= max_trades:
                break
            if stop_at is None or asyncio.get_running_loop().time() >= stop_at:
                break
            await asyncio.sleep(poll_s)

    ranked = sorted(
        stats.values(),
        key=lambda row: (row.target_volume, Decimal(row.target_fills), Decimal(-row.input_rank)),
        reverse=True,
    )
    return ranked


async def fetch_user_twap_only_record(
    client: httpx.AsyncClient,
    info_url: str,
    stats: UserRankStats,
) -> tuple[UserRankStats, list[Any]]:
    twap_data = await post_info(client, info_url, {"type": "userTwapSliceFills", "user": stats.user})
    twap_fills = twap_data if isinstance(twap_data, list) else []
    stats.twap_fills_seen = len(twap_fills)
    return stats, twap_fills


async def fetch_user_total_fills_record(
    client: httpx.AsyncClient,
    info_url: str,
    stats: UserRankStats,
) -> tuple[UserRankStats, list[Any]]:
    fills_data = await post_info(client, info_url, {"type": "userFills", "user": stats.user})
    fills = fills_data if isinstance(fills_data, list) else []
    return stats, fills


async def gather_user_twap_only_records(
    stats_rows: list[UserRankStats],
    info_url: str,
    concurrency: int,
) -> list[tuple[UserRankStats, list[Any]]]:
    sem = asyncio.Semaphore(concurrency)
    async with httpx.AsyncClient(timeout=20.0) as client:

        async def one(stats: UserRankStats) -> tuple[UserRankStats, list[Any]]:
            async with sem:
                return await fetch_user_twap_only_record(client, info_url, stats)

        return await asyncio.gather(*(one(stats) for stats in stats_rows))


async def gather_user_total_fill_records(
    stats_rows: list[UserRankStats],
    info_url: str,
    concurrency: int,
) -> list[tuple[UserRankStats, list[Any]]]:
    sem = asyncio.Semaphore(concurrency)
    async with httpx.AsyncClient(timeout=20.0) as client:

        async def one(stats: UserRankStats) -> tuple[UserRankStats, list[Any]]:
            async with sem:
                return await fetch_user_total_fills_record(client, info_url, stats)

        return await asyncio.gather(*(one(stats) for stats in stats_rows))


async def gather_user_sample_records(
    users: list[str],
    info_url: str,
    coin: str,
    ranking_window: tuple[int, int],
    rank_by: str,
    concurrency: int,
) -> list[tuple[UserRankStats, list[Any]]]:
    need_account_value = rank_by == "account_value"
    sem = asyncio.Semaphore(concurrency)
    async with httpx.AsyncClient(timeout=20.0) as client:

        async def one(idx: int, user: str) -> tuple[UserRankStats, list[Any]]:
            async with sem:
                return await fetch_user_sample_record(
                    client,
                    info_url,
                    user,
                    idx,
                    coin,
                    ranking_window,
                    need_account_value,
                )

        return await asyncio.gather(*(one(idx, user) for idx, user in enumerate(users)))


async def compare_user_sample_twap_fills(
    users_file: str | None,
    discover_users: str,
    coin: str,
    info_url: str,
    rank_by: str,
    top_n: int,
    ranking_window_s: int,
    window_s: int,
    compare_offset_s: int,
    concurrency: int,
    candidate_limit: int,
    discover_seconds: int,
    discover_poll_s: float,
    discover_max_trades: int,
    report_mode: str,
) -> dict[str, Any]:
    users: list[str] = []
    discovered_stats: dict[str, UserRankStats] = {}
    if users_file:
        users.extend(read_user_addresses(users_file))
    if discover_users == "recent-trades":
        discovered = await discover_recent_trade_users(
            info_url=info_url,
            coin=coin,
            seconds=discover_seconds,
            poll_s=discover_poll_s,
            max_trades=discover_max_trades,
        )
        discovered_stats = {row.user: row for row in discovered}
        seen = set(users)
        users.extend(row.user for row in discovered if row.user not in seen)
    if not users:
        raise RuntimeError("no valid candidate users found; pass --sample-users-file or use --discover-users")
    if candidate_limit > 0:
        users = users[:candidate_limit]

    now_ms = utc_now_ms()
    current = (now_ms - window_s * 1000, now_ms)
    past_end = now_ms - compare_offset_s * 1000
    past = (past_end - window_s * 1000, past_end)
    ranking_window = (now_ms - ranking_window_s * 1000, now_ms)
    windows = {"current": current, "past": past}

    can_rank_from_discovery = (
        discover_users == "recent-trades"
        and rank_by in {"target_volume", "target_fills", "input_order"}
        and not users_file
    )
    if can_rank_from_discovery:
        ranked_stats = [discovered_stats[user] for user in users if user in discovered_stats]
        ranked_stats.sort(
            key=lambda row: (row.score(rank_by), Decimal(-row.input_rank)),
            reverse=True,
        )
        records = await gather_user_twap_only_records(
            stats_rows=ranked_stats[:top_n],
            info_url=info_url,
            concurrency=concurrency,
        )
    else:
        records = await gather_user_sample_records(
            users=users,
            info_url=info_url,
            coin=coin,
            ranking_window=ranking_window,
            rank_by=rank_by,
            concurrency=concurrency,
        )
    records.sort(key=lambda row: (row[0].score(rank_by), Decimal(-row[0].input_rank)), reverse=True)
    selected = records[:top_n]

    aggs = {"current": empty_side_aggs(), "past": empty_side_aggs()}
    user_aggs: dict[str, dict[str, dict[str, WindowAgg]]] = {
        stats.user: {"current": empty_side_aggs(), "past": empty_side_aggs()}
        for stats, _twap_fills in selected
    }
    for stats, twap_fills in selected:
        for raw in twap_fills:
            ingest_user_twap_fill(raw, coin, windows, aggs, user_aggs, stats.user)

    total_aggs: dict[str, dict[str, WindowAgg]] | None = None
    if report_mode in {"twap-share", "all"}:
        total_aggs = {"current": empty_side_aggs(), "past": empty_side_aggs()}
        total_records = await gather_user_total_fill_records(
            stats_rows=[stats for stats, _twap_fills in selected],
            info_url=info_url,
            concurrency=concurrency,
        )
        for _stats, fills in total_records:
            for raw in fills:
                ingest_user_total_fill(raw, coin, windows, total_aggs)

    by_side: dict[str, dict[str, Any]] = {}
    for side in ("B", "A", "unknown"):
        row: dict[str, Any] = {
            "current": aggs["current"][side].to_output(),
            "past": aggs["past"][side].to_output(),
            "size_ratio_current_over_past": window_ratio(
                aggs["current"][side].size, aggs["past"][side].size
            ),
            "notional_ratio_current_over_past": window_ratio(
                aggs["current"][side].notional, aggs["past"][side].notional
            ),
        }
        if total_aggs is not None:
            current_total = total_aggs["current"][side]
            past_total = total_aggs["past"][side]
            current_share = window_share(aggs["current"][side].notional, current_total.notional)
            past_share = window_share(aggs["past"][side].notional, past_total.notional)
            row.update(
                {
                    "sampled_total_current": current_total.to_output(),
                    "sampled_total_past": past_total.to_output(),
                    "twap_notional_share_current": current_share,
                    "twap_notional_share_past": past_share,
                    "twap_notional_share_ratio_current_over_past": (
                        window_ratio(current_share, past_share)
                        if current_share is not None and past_share is not None
                        else None
                    ),
                }
            )
        by_side[side] = row

    imbalance = None
    if report_mode == "all":
        current_imbalance = twap_notional_imbalance(aggs["current"])
        past_imbalance = twap_notional_imbalance(aggs["past"])
        imbalance = {
            "current": current_imbalance,
            "past": past_imbalance,
            "current_minus_past": (
                current_imbalance - past_imbalance
                if current_imbalance is not None and past_imbalance is not None
                else None
            ),
            "definition": "(BUY_TWAP_NOTIONAL - SELL_TWAP_NOTIONAL) / "
            "(BUY_TWAP_NOTIONAL + SELL_TWAP_NOTIONAL)",
        }

    return {
        "coin": coin,
        "method": "sampled userTwapSliceFills aggregation",
        "report_mode": report_mode,
        "sample": {
            "users_file": users_file,
            "discover_users": discover_users,
            "discover_seconds": discover_seconds,
            "discover_poll_s": discover_poll_s,
            "discover_max_trades": discover_max_trades,
            "candidate_users": len(users),
            "candidate_limit": candidate_limit,
            "selected_users": len(selected),
            "rank_by": rank_by,
            "ranking_window": {
                "start": iso_from_ms(ranking_window[0]),
                "end": iso_from_ms(ranking_window[1]),
            },
            "top_users": [stats.to_output(rank_by) for stats, _twap_fills in selected],
        },
        "windows": {
            "current": {"start": iso_from_ms(current[0]), "end": iso_from_ms(current[1])},
            "past": {"start": iso_from_ms(past[0]), "end": iso_from_ms(past[1])},
        },
        "by_side": by_side,
        "twap_notional_imbalance": imbalance,
        "by_user": {
            user: {
                window: {
                    side: side_agg.to_output()
                    for side, side_agg in side_aggs.items()
                    if side_agg.fills > 0
                }
                for window, side_aggs in window_aggs.items()
            }
            for user, window_aggs in user_aggs.items()
        },
    }


def order_to_output(order: TwapOrder, fills: FillAgg | None) -> dict[str, Any]:
    avg_interval = fills.avg_interval_s if fills else None
    vwap = fills.vwap if fills else None
    return {
        "twap_id": order.twap_id,
        "side": order.side,
        "direction": side_label(order.side),
        "status": order.status,
        "total_size": str(order.total_size),
        "executed_size": str(order.executed_size),
        "remaining_size": str(order.remaining_size),
        "minutes": str(order.minutes),
        "randomize": order.randomize,
        "reduce_only": order.reduce_only,
        "created_at": iso_from_ms(order.created_ms),
        "expected_end": iso_from_ms(order.end_ms),
        "planned_interval_s": str(HL_NATIVE_TWAP_INTERVAL_S),
        "planned_slice_size": str(order.planned_slice_size),
        "planned_size_per_min": str(order.planned_size_per_min),
        "observed_fill_count": fills.count if fills else 0,
        "observed_fill_size": str(fills.total_size if fills else Decimal("0")),
        "observed_avg_interval_s": str(avg_interval) if avg_interval is not None else None,
        "observed_vwap": str(vwap) if vwap is not None else None,
    }


async def seed_latest_blocks(http_url: str, stream: str, count: int, monitor: TwapMonitor) -> None:
    payload = {
        "jsonrpc": "2.0",
        "method": "hl_getLatestBlocks",
        "params": {"stream": stream, "count": count},
        "id": 1,
    }
    async with httpx.AsyncClient(timeout=20.0) as client:
        resp = await client.post(http_url, json=payload)
        resp.raise_for_status()
        data = resp.json()
    if "error" in data:
        raise RuntimeError(f"hl_getLatestBlocks error: {data['error']}")
    events = normalize_stream_events(data)
    for event in events:
        if isinstance(event, dict):
            monitor.ingest_twap_event(event)


async def json_rpc(client: httpx.AsyncClient, http_url: str, method: str, params: dict[str, Any]) -> Any:
    resp = await client.post(
        http_url,
        json={"jsonrpc": "2.0", "method": method, "params": params, "id": 1},
    )
    resp.raise_for_status()
    data = resp.json()
    if "error" in data:
        raise RuntimeError(f"{method} error: {data['error']}")
    return data


def latest_block_number_from_response(data: dict[str, Any]) -> int:
    result = data.get("result")
    if isinstance(result, int):
        return result
    if isinstance(result, str) and result.isdigit():
        return int(result)
    if isinstance(result, dict):
        for key in ("block", "blockNumber", "height", "number"):
            value = result.get(key)
            if isinstance(value, int):
                return value
            if isinstance(value, str) and value.isdigit():
                return int(value)
    raise RuntimeError(f"cannot parse latest block number response: {data}")


async def compare_historical_twap_fills(
    http_url: str,
    coin: str,
    window_s: int,
    compare_offset_s: int,
    blocks_per_second: Decimal,
    batch_blocks: int,
    max_blocks: int,
) -> dict[str, Any]:
    now_ms = utc_now_ms()
    current = (now_ms - window_s * 1000, now_ms)
    past_end = now_ms - compare_offset_s * 1000
    past = (past_end - window_s * 1000, past_end)
    windows = {"current": current, "past": past}
    aggs = {"current": empty_side_aggs(), "past": empty_side_aggs()}

    seconds_to_cover = compare_offset_s + window_s
    blocks_to_fetch = int((Decimal(seconds_to_cover) * blocks_per_second).to_integral_value()) + batch_blocks
    if blocks_to_fetch > max_blocks:
        raise RuntimeError(
            f"requested historical range needs ~{blocks_to_fetch} blocks; "
            f"increase --max-blocks if intentional"
        )

    async with httpx.AsyncClient(timeout=30.0) as client:
        latest_data = await json_rpc(client, http_url, "hl_getLatestBlockNumber", {"stream": "trades"})
        latest_block = latest_block_number_from_response(latest_data)
        from_block = max(0, latest_block - blocks_to_fetch)
        scanned_blocks = 0
        for start in range(from_block, latest_block + 1, batch_blocks):
            end = min(latest_block, start + batch_blocks - 1)
            data = await json_rpc(
                client,
                http_url,
                "hl_getBatchBlocks",
                {"stream": "trades", "from": start, "to": end},
            )
            blocks = normalize_blocks(data)
            scanned_blocks += len(blocks) if blocks else end - start + 1
            events = normalize_stream_events(data)
            for event in events:
                ingest_historical_trade(event, coin, windows, aggs)

    return {
        "coin": coin,
        "stream": "trades",
        "method": "twapId fill aggregation",
        "block_estimate": {
            "blocks_per_second": blocks_per_second,
            "blocks_requested": blocks_to_fetch,
            "blocks_scanned": scanned_blocks,
            "batch_blocks": batch_blocks,
        },
        "windows": {
            "current": {"start": iso_from_ms(current[0]), "end": iso_from_ms(current[1])},
            "past": {"start": iso_from_ms(past[0]), "end": iso_from_ms(past[1])},
        },
        "by_side": {
            side: {
                "current": aggs["current"][side].to_output(),
                "past": aggs["past"][side].to_output(),
                "size_ratio_current_over_past": window_ratio(
                    aggs["current"][side].size, aggs["past"][side].size
                ),
                "notional_ratio_current_over_past": window_ratio(
                    aggs["current"][side].notional, aggs["past"][side].notional
                ),
            }
            for side in ("B", "A", "unknown")
        },
    }


async def subscribe_ws(
    ws_url: str,
    twap_stream: str,
    monitor: TwapMonitor,
    seconds: int,
    print_every_s: int,
    window_s: int,
    json_output: bool,
) -> None:
    stop_at = asyncio.get_running_loop().time() + seconds if seconds > 0 else None
    next_print = asyncio.get_running_loop().time()

    async with websockets.connect(ws_url, ping_interval=20, ping_timeout=20) as ws:
        await ws.send(
            json.dumps(
                {
                    "jsonrpc": "2.0",
                    "method": "hl_subscribe",
                    "params": {
                        "streamType": twap_stream,
                        "filters": {"coin": [monitor.coin]},
                        "filterName": f"{monitor.coin.lower()}_twaps",
                    },
                    "id": 1,
                }
            )
        )
        await ws.send(
            json.dumps(
                {
                    "jsonrpc": "2.0",
                    "method": "hl_subscribe",
                    "params": {
                        "streamType": "trades",
                        "filters": {"coin": [monitor.coin], "twapId": ["*"]},
                        "filterName": f"{monitor.coin.lower()}_twap_fills",
                    },
                    "id": 2,
                }
            )
        )

        while True:
            now = asyncio.get_running_loop().time()
            if stop_at is not None and now >= stop_at:
                break
            if now >= next_print:
                print_snapshot(monitor, window_s, json_output)
                next_print = now + print_every_s
            timeout = max(0.1, min(1.0, next_print - now))
            try:
                raw = await asyncio.wait_for(ws.recv(), timeout=timeout)
            except TimeoutError:
                continue
            msg = json.loads(raw)
            if "error" in msg:
                raise RuntimeError(f"subscription error: {msg['error']}")
            stream = str(msg.get("stream") or msg.get("channel") or "").lower()
            events = normalize_stream_events(msg)
            if not events:
                continue
            is_trade_stream = stream == "trades" or any(
                isinstance(e, list | tuple) and len(e) >= 2 for e in events
            )
            for event in events:
                if is_trade_stream:
                    monitor.ingest_trade_event(event)
                elif isinstance(event, dict):
                    monitor.ingest_twap_event(event)


def print_snapshot(monitor: TwapMonitor, window_s: int, json_output: bool) -> None:
    snap = monitor.snapshot(window_s)
    if json_output:
        print(json.dumps(to_jsonable(snap), ensure_ascii=False, sort_keys=True))
        return

    print(f"\n[{snap['now']}] {snap['coin']} public TWAP summary")
    print(f"known_orders={snap['known_orders']} known_twap_ids_with_fills={snap['known_twap_ids_with_fills']}")

    print("planned active:")
    planned = snap["planned_by_side"]
    if not planned:
        print("  none observed")
    for side, row in planned.items():
        print(
            "  "
            f"{side} {side_label(side)} | orders={row['orders']} "
            f"remaining={row['remaining_size']} "
            f"rate={row['planned_size_per_min']}/min "
            f"expected_30s_slice_sum={row['planned_slice_size_sum']}"
        )

    actual_key = f"actual_fills_last_{window_s}s_by_side"
    print(f"actual fills last {window_s}s:")
    actual = snap[actual_key]
    if not actual:
        print("  none observed")
    for side, row in actual.items():
        print(
            "  "
            f"{side} {side_label(side)} | fills={row['fills']} "
            f"size={row['size']} notional={row['notional']}"
        )

    active_orders = snap["active_orders"]
    if active_orders:
        print("active order details:")
        for row in active_orders[:20]:
            print(
                "  "
                f"twap={row['twap_id']} {row['side']} "
                f"total={row['total_size']} remaining={row['remaining_size']} "
                f"minutes={row['minutes']} planned_slice={row['planned_slice_size']} "
                f"randomize={row['randomize']} end={row['expected_end']} "
                f"observed_fills={row['observed_fill_count']}"
            )
        if len(active_orders) > 20:
            print(f"  ... {len(active_orders) - 20} more")


def print_historical_comparison(result: dict[str, Any], json_output: bool) -> None:
    if json_output:
        print(json.dumps(to_jsonable(result), ensure_ascii=False, sort_keys=True))
        return

    windows = result["windows"]
    print(f"\n{result['coin']} public TWAP fill comparison")
    print(f"current: {windows['current']['start']} -> {windows['current']['end']}")
    print(f"past   : {windows['past']['start']} -> {windows['past']['end']}")
    estimate = result.get("block_estimate")
    if estimate:
        print(
            "scan   : "
            f"~{estimate['blocks_requested']} requested blocks, "
            f"{estimate['blocks_scanned']} scanned, "
            f"{estimate['blocks_per_second']} blocks/s estimate"
        )
    sample = result.get("sample")
    if isinstance(sample, dict):
        print(
            "sample : "
            f"{sample['selected_users']}/{sample['candidate_users']} users, "
            f"rank_by={sample['rank_by']}, "
            f"discover={sample['discover_users']}"
        )
        if sample.get("top_users"):
            print("top users:")
            for row in sample["top_users"][:10]:
                print(
                    "  "
                    f"{row['user']} score={row['score']} "
                    f"target_volume={row['target_volume']} "
                    f"target_fills={row['target_fills']} "
                    f"all_volume={row['all_volume']} "
                    f"all_fills={row['all_fills']} "
                    f"account_value={row['account_value']}"
                )
    for side, row in result["by_side"].items():
        current = row["current"]
        past = row["past"]
        print(f"\n{side} {side_label(side)}")
        print(
            "  current | "
            f"fills={current['fills']} twaps={current['twaps']} "
            f"size={current['size']} notional={current['notional']} "
            f"avg_interval_s={current['avg_interval_s']} vwap={current['vwap']}"
        )
        print(
            "  past    | "
            f"fills={past['fills']} twaps={past['twaps']} "
            f"size={past['size']} notional={past['notional']} "
            f"avg_interval_s={past['avg_interval_s']} vwap={past['vwap']}"
        )
        print(
            "  ratio   | "
            f"size={row['size_ratio_current_over_past']} "
            f"notional={row['notional_ratio_current_over_past']}"
        )
        if "sampled_total_current" in row:
            total_current = row["sampled_total_current"]
            total_past = row["sampled_total_past"]
            print(
                "  total   | "
                f"current_fills={total_current['fills']} "
                f"current_notional={total_current['notional']} "
                f"past_fills={total_past['fills']} "
                f"past_notional={total_past['notional']}"
            )
            print(
                "  share   | "
                f"current={row['twap_notional_share_current']} "
                f"past={row['twap_notional_share_past']} "
                f"ratio={row['twap_notional_share_ratio_current_over_past']}"
            )
    imbalance = result.get("twap_notional_imbalance")
    if isinstance(imbalance, dict):
        print("\nTWAP notional imbalance")
        print(f"  definition: {imbalance['definition']}")
        print(
            "  value     : "
            f"current={imbalance['current']} "
            f"past={imbalance['past']} "
            f"delta={imbalance['current_minus_past']}"
        )


def to_jsonable(value: Any) -> Any:
    if isinstance(value, Decimal):
        return str(value)
    if isinstance(value, dict):
        return {str(k): to_jsonable(v) for k, v in value.items()}
    if isinstance(value, list):
        return [to_jsonable(v) for v in value]
    if hasattr(value, "__dataclass_fields__"):
        return to_jsonable(asdict(value))
    return value


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--coin", required=True, help="Coin to monitor, e.g. HYPE, BTC, xyz:NVDA, @107")
    parser.add_argument(
        "--ws-url",
        default=os.environ.get("HL_PUBLIC_STREAM_WS_URL"),
        help="Provider WebSocket URL that supports hl_subscribe.",
    )
    parser.add_argument(
        "--http-url",
        default=os.environ.get("HL_PUBLIC_STREAM_HTTP_URL"),
        help="Optional provider HTTP URL that supports hl_getLatestBlocks for seeding.",
    )
    parser.add_argument(
        "--info-url",
        default=os.environ.get("HL_INFO_URL", HL_INFO_URL),
        help="Official/free Hyperliquid info endpoint URL.",
    )
    parser.add_argument(
        "--twap-stream",
        default=os.environ.get("HL_PUBLIC_TWAP_STREAM", "twap"),
        help="TWAP stream name for provider JSON-RPC (default: twap).",
    )
    parser.add_argument(
        "--seed-blocks",
        type=int,
        default=0,
        help="Seed from latest TWAP blocks before live monitoring. Requires --http-url.",
    )
    parser.add_argument(
        "--seconds",
        type=int,
        default=0,
        help="Monitoring duration. 0 means run until interrupted.",
    )
    parser.add_argument(
        "--print-every-s",
        type=int,
        default=30,
        help="Summary print interval in seconds.",
    )
    parser.add_argument(
        "--window-s",
        type=int,
        default=3_600,
        help="Window size for fill aggregation in seconds. Default: 1h.",
    )
    parser.add_argument(
        "--compare-history",
        action="store_true",
        help="One-shot comparison of current window vs the same window ending --compare-offset-s ago.",
    )
    parser.add_argument(
        "--user-sample",
        action="store_true",
        help=(
            "Compare TWAP fills for a sampled user set using free official info endpoints. "
            "Use --discover-users recent-trades or --sample-users-file."
        ),
    )
    parser.add_argument(
        "--discover-users",
        default="none",
        choices=["none", "recent-trades"],
        help="Free candidate user discovery source. recent-trades uses official coin recentTrades users.",
    )
    parser.add_argument(
        "--discover-seconds",
        type=int,
        default=0,
        help="Seconds to poll recentTrades for candidate users. 0 means one request.",
    )
    parser.add_argument(
        "--discover-poll-s",
        type=float,
        default=2.0,
        help="Polling interval for --discover-users recent-trades.",
    )
    parser.add_argument(
        "--discover-max-trades",
        type=int,
        default=500,
        help="Stop recentTrades discovery after this many unique trades. 0 means no cap.",
    )
    parser.add_argument(
        "--sample-users-file",
        default=None,
        help="Optional newline/CSV/JSON address list. Combined with discovered users when both are set.",
    )
    parser.add_argument(
        "--sample-rank-by",
        default="target_volume",
        choices=[
            "target_volume",
            "target_fills",
            "all_volume",
            "all_fills",
            "account_value",
            "input_order",
        ],
        help="How to rank candidate users before selecting --sample-top-n.",
    )
    parser.add_argument(
        "--sample-top-n",
        type=int,
        default=100,
        help="Number of ranked users to include in user-sampled TWAP aggregation.",
    )
    parser.add_argument(
        "--sample-candidate-limit",
        type=int,
        default=100,
        help="Maximum candidate users to query after discovery/file input. 0 means no cap.",
    )
    parser.add_argument(
        "--ranking-window-s",
        type=int,
        default=86_400,
        help="Window for userFills ranking metrics. Default: 24h.",
    )
    parser.add_argument(
        "--sample-concurrency",
        type=int,
        default=3,
        help="Concurrent official info requests for user-sampled mode.",
    )
    parser.add_argument(
        "--sample-report-mode",
        default="twap-notional",
        choices=["twap-notional", "twap-share", "all"],
        help=(
            "User-sampled report mode. twap-notional keeps the existing TWAP current/past "
            "notional comparison; twap-share also fetches sampled total volume and reports "
            "TWAP share; all additionally reports BUY/SELL TWAP notional imbalance."
        ),
    )
    parser.add_argument(
        "--compare-offset-s",
        type=int,
        default=43_200,
        help="Historical comparison offset in seconds. Default: 12h.",
    )
    parser.add_argument(
        "--blocks-per-second",
        type=Decimal,
        default=DEFAULT_BLOCKS_PER_SECOND,
        help="Block-rate estimate used to backfill historical trades. Default: 12.",
    )
    parser.add_argument(
        "--batch-blocks",
        type=int,
        default=200,
        help="Block count per hl_getBatchBlocks request.",
    )
    parser.add_argument(
        "--max-blocks",
        type=int,
        default=700_000,
        help="Safety cap for historical scan size.",
    )
    parser.add_argument("--json", action="store_true", help="Print machine-readable JSON lines.")
    args = parser.parse_args()

    if not args.compare_history and not args.user_sample and not args.ws_url:
        parser.error("--ws-url or HL_PUBLIC_STREAM_WS_URL is required")
    if args.compare_history and not args.http_url:
        parser.error("--compare-history requires --http-url or HL_PUBLIC_STREAM_HTTP_URL")
    if args.user_sample and args.discover_users == "none" and not args.sample_users_file:
        parser.error("--user-sample requires --discover-users recent-trades or --sample-users-file")
    if args.seed_blocks > 0 and not args.http_url:
        parser.error("--seed-blocks requires --http-url or HL_PUBLIC_STREAM_HTTP_URL")
    if args.print_every_s <= 0:
        parser.error("--print-every-s must be > 0")
    if args.window_s <= 0:
        parser.error("--window-s must be > 0")
    if args.compare_offset_s <= 0:
        parser.error("--compare-offset-s must be > 0")
    if args.blocks_per_second <= 0:
        parser.error("--blocks-per-second must be > 0")
    if args.batch_blocks <= 0:
        parser.error("--batch-blocks must be > 0")
    if args.max_blocks <= 0:
        parser.error("--max-blocks must be > 0")
    if args.discover_seconds < 0:
        parser.error("--discover-seconds must be >= 0")
    if args.discover_poll_s <= 0:
        parser.error("--discover-poll-s must be > 0")
    if args.discover_max_trades < 0:
        parser.error("--discover-max-trades must be >= 0")
    if args.sample_top_n <= 0:
        parser.error("--sample-top-n must be > 0")
    if args.sample_candidate_limit < 0:
        parser.error("--sample-candidate-limit must be >= 0")
    if args.ranking_window_s <= 0:
        parser.error("--ranking-window-s must be > 0")
    if args.sample_concurrency <= 0:
        parser.error("--sample-concurrency must be > 0")
    return args


async def amain() -> None:
    args = parse_args()
    if args.user_sample:
        result = await compare_user_sample_twap_fills(
            users_file=args.sample_users_file,
            discover_users=args.discover_users,
            coin=args.coin,
            info_url=args.info_url,
            rank_by=args.sample_rank_by,
            top_n=args.sample_top_n,
            ranking_window_s=args.ranking_window_s,
            window_s=args.window_s,
            compare_offset_s=args.compare_offset_s,
            concurrency=args.sample_concurrency,
            candidate_limit=args.sample_candidate_limit,
            discover_seconds=args.discover_seconds,
            discover_poll_s=args.discover_poll_s,
            discover_max_trades=args.discover_max_trades,
            report_mode=args.sample_report_mode,
        )
        print_historical_comparison(result, args.json)
        return

    if args.compare_history:
        result = await compare_historical_twap_fills(
            http_url=args.http_url,
            coin=args.coin,
            window_s=args.window_s,
            compare_offset_s=args.compare_offset_s,
            blocks_per_second=args.blocks_per_second,
            batch_blocks=args.batch_blocks,
            max_blocks=args.max_blocks,
        )
        print_historical_comparison(result, args.json)
        return

    monitor = TwapMonitor(args.coin)
    if args.seed_blocks > 0:
        await seed_latest_blocks(args.http_url, args.twap_stream, args.seed_blocks, monitor)
        print_snapshot(monitor, args.window_s, args.json)
    await subscribe_ws(
        ws_url=args.ws_url,
        twap_stream=args.twap_stream,
        monitor=monitor,
        seconds=args.seconds,
        print_every_s=args.print_every_s,
        window_s=args.window_s,
        json_output=args.json,
    )
    print_snapshot(monitor, args.window_s, args.json)


def main() -> None:
    loop = asyncio.new_event_loop()
    asyncio.set_event_loop(loop)
    for sig in (signal.SIGINT, signal.SIGTERM):
        with suppress(NotImplementedError):
            loop.add_signal_handler(sig, loop.stop)
    try:
        loop.run_until_complete(amain())
    except KeyboardInterrupt:
        pass
    except RuntimeError as exc:
        print(f"error: {exc}", file=sys.stderr)
        sys.exit(1)
    finally:
        loop.close()


if __name__ == "__main__":
    main()
