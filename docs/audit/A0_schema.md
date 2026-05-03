# Audit-A0: schema sanity check

raw_root: `data/raw`
all_healthy: **True**

## l2book

- files: 178
- rows: 79240
- healthy: True

### Columns

| column | dtype | tz-aware |
|---|---|---|
| ask_ns | List(Int64) | - |
| ask_pxs | List(Float64) | - |
| ask_szs | List(Float64) | - |
| best_ask | Float64 | - |
| best_bid | Float64 | - |
| bid_ns | List(Int64) | - |
| bid_pxs | List(Float64) | - |
| bid_szs | List(Float64) | - |
| date | Date | - |
| exchange_ts | Datetime(time_unit='us', time_zone='Asia/Tokyo') | yes |
| hour | Int64 | - |
| is_recovery_snapshot | Boolean | - |
| mid | Float64 | - |
| recv_ts | Datetime(time_unit='us', time_zone='Asia/Tokyo') | yes |
| symbol | String | - |

## trades

- files: 178
- rows: 53834
- healthy: True

### Columns

| column | dtype | tz-aware |
|---|---|---|
| buyer | String | - |
| date | Date | - |
| exchange_ts | Datetime(time_unit='us', time_zone='Asia/Tokyo') | yes |
| hash | String | - |
| hour | Int64 | - |
| px | Float64 | - |
| recv_ts | Datetime(time_unit='us', time_zone='Asia/Tokyo') | yes |
| seller | String | - |
| side | String | - |
| symbol | String | - |
| sz | Float64 | - |
| trade_id | String | - |

## asset_ctxs

- files: 178
- rows: 712
- healthy: True

### Columns

| column | dtype | tz-aware |
|---|---|---|
| date | Date | - |
| day_base_volume | Float64 | - |
| day_volume | Float64 | - |
| dex | String | - |
| funding_rate | Float64 | - |
| hour | Int64 | - |
| impact_ask_px | Float64 | - |
| impact_bid_px | Float64 | - |
| mark_px | Float64 | - |
| mid_px | Float64 | - |
| open_interest | Float64 | - |
| oracle_px | Float64 | - |
| poll_ts | Datetime(time_unit='us', time_zone='Asia/Tokyo') | yes |
| premium | Float64 | - |
| prev_day_px | Float64 | - |
| symbol | String | - |
