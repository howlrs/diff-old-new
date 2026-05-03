# Audit-D: data quality score

0-100 score (internal 70 + external 30). 95 未満で warning, 80 未満で critical.

## summary

| symbol | score | warning |
|---|---|---|
| BTC | 100.0 | ✓ |
| ETH | 100.0 | ✓ |
| xyz:SP500 | 95.0 | ✓ |
| xyz:XYZ100 | 95.0 | ✓ |

## xyz:SP500 deductions

| rule | points | message |
|---|---|---|
| external_market_closed | -5.0 | external market closed (expected) |

## xyz:XYZ100 deductions

| rule | points | message |
|---|---|---|
| external_market_closed | -5.0 | external market closed (expected) |
