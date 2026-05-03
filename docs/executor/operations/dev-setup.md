# 開発環境セットアップ

## 前提

| ツール | バージョン | 備考 |
|---|---|---|
| Rust | stable (1.85+) | `dtolnay/rust-toolchain@stable` で CI 同等 |
| cargo | 同梱 | rustup で導入 |
| Python | 3.12 系 | pyenv / asdf 推奨 |
| Linux/WSL2 | 任意 | macOS でも動く想定だが CI は Ubuntu |

## 初回セットアップ

### 1. Rust workspace

```bash
cd executor
cargo build --workspace
```

初回は依存解決で 200+ crate コンパイルされるため数分かかる。
失敗するなら `cargo clean && cargo build --workspace` で.

### 2. Python venv

```bash
cd /path/to/diff-old-new
python -m venv .venv
source .venv/bin/activate
pip install -e ".[dev,all]"
```

`all` extra は GUI/audit 含む meta extra。 `dev` だけだと一部テストが skip される。

## ビルドターゲット早見表

| コマンド | やること |
|---|---|
| `cargo build` (executor 配下) | dev profile で全 crate ビルド |
| `cargo build --release -p executor-server` | リリースバイナリのみ |
| `cargo build -p executor-cli` | CLI のみ |
| `cargo test --workspace --all-features` | 全 Rust test (CI と同じ) |
| `cargo test -p executor-algo market_make::tests` | 単一モジュール |
| `cargo fmt --all -- --check` | format 検証 (CI と同じ) |
| `cargo clippy --workspace --all-targets -- -D warnings` | lint (CI と同じ) |

## テスト分類

### Rust

| layer | crate | 件数 (2026-05-04) |
|---|---|---|
| core types | executor-core | 22 |
| HL 基盤 | executor-hl | 17 |
| algorithms | executor-algo | 56 |
| server unit | executor-server | 8 |
| server integration | executor-server (`tests/`) | 10 |
| **計** | | **113** |

### Python

| layer | path | 件数 |
|---|---|---|
| 既存 (audit/strategy/etc) | `tests/test_*` | 71 |
| executor connector unit | `tests/test_executor_client.py` | 8 |
| executor connector live e2e | `tests/test_executor_client_live.py` | 5 (marker `live`) |

CI はデフォルト `pytest -m "not live and not slow"` → 79 件実行。

## ローカル CI 再現

`scripts/check_ci_local.sh` が両方走らせる:

```bash
bash scripts/check_ci_local.sh
```

中身:
1. Python: `ruff check` / `ruff format --check` / `mypy` / `pytest -m "not live and not slow"`
2. Rust: `cargo fmt --check` / `cargo clippy -D warnings` / `cargo test --workspace`

CI 失敗を防ぎたい場合は push 前に必ず実行 (CONTRIBUTING.md にも記載)。

## サーバを起動して触る

```bash
cd executor
cargo run -p executor-server --release

# 別 terminal
curl http://127.0.0.1:8085/v1/health | jq
```

`MockHlClient` + `MockSigner` なので keyless で動く。
板情報は WS 未接続なので空。`POST /v1/exec` で MARKET algo を kick すると
`market: empty asks` エラーで slice が失敗する → これが期待挙動。

書き込みたい場合は **テスト用 fixture** を参照。`tests/integration_rest.rs` の
`build_state_with_seed()` が AppState に手動で book / position をシードしている。

## live e2e を回す

```bash
cd executor && cargo build -p executor-server   # debug build で OK
cd ..
source .venv/bin/activate
pytest tests/test_executor_client_live.py -m live -v
```

5 ケース 1 秒未満で完了。fixture が free port を取り subprocess.Popen → /v1/health poll → tear down。

## ディレクトリ早見表

```
diff-old-new/
├── executor/                           # Rust workspace (8 PR で実装済)
│   ├── Cargo.toml
│   └── crates/{executor-core, executor-hl, executor-algo, executor-server, executor-cli}/
├── src/
│   ├── executor/                       # Python connector (PR-8)
│   ├── audit/, l1_collector/, l2_features/, l3_strategy/, gui/, ...
│   └── ...
├── tests/                              # pytest
│   ├── test_executor_client.py         # connector unit (CI 対象)
│   ├── test_executor_client_live.py    # e2e live (marker live)
│   └── test_*.py (audit/strategy/etc)
├── docs/
│   ├── executor/                       # ★ このディレクトリ
│   ├── specs/2026-05-04-rust-executor-design.md
│   ├── audit/, kpi/, ...
│   └── phase-1-report.md
└── scripts/check_ci_local.sh
```

## トラブルシューティング

[`troubleshooting.md`](troubleshooting.md) を参照。
