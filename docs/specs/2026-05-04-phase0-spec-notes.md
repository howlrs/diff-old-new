# Phase 0 調査結果: Hyperliquid 米株 perp 仕様メモ
調査日: 2026-05-04

## 1. 重要な構造的事実 (戦略設計に決定的影響)

### 1.1 Hyperliquid の SPX/NDX 系 perp は **HIP-3 経由の builder-deployed perp**
- 標準のcrypto perp (BTC, ETH等) と **同じ取引所内にあるが仕組みが違う**
- デプロイヤー: **Trade[XYZ]** (S&P Dow Jones Indices 公式ライセンス取得)
- 2026-03-18 にS&P 500の公式ライセンス契約を発表
- Trade[XYZ] が独自にOracle価格を提供する (validator公式ではない)

### 1.2 「Hyperliquid の S&P 500 perp」の正式仕様
| 項目 | 値 |
|---|---|
| Hyperliquid ticker | **SP500** (SPXではない) |
| Oracle source (active session) | EMM6 / USD (extended session時) ・ SPX cash index (US現物時間) |
| Oracle source (closure) | 内部EMA機構 (連続時間指数加重平均) |
| Update frequency | リレイヤーが約3秒ごと |
| Funding interval | 1時間ごと |
| Funding multiplier | 0.5x (HL標準のクリプトperpの半分、SOFR + 1-2% 相当) |
| Funding cap | ±4%/hour |
| Trading session (US equities) | 日曜 20:00 ET 〜 金曜 20:00 ET |
| Closure window | 金曜 20:00 〜 日曜 20:00 ET (49時間) |
| Oracle data provider | 「institutional liquidity providers, partners such as Pyth」 |

### 1.3 利用可能な perp 一覧 (Trade[XYZ] 経由、HIP-3)
- **広域インデックス**: SP500, XYZ100 (Nasdaq 100相当), JP225, KR200
- **米個別株**: TSLA, NVDA, GOOGL, INTC, MU, PLTR, ORCL, MSTR, MSFT, META, AMZN, AMD, AAPL, COIN, HOOD, NFLX, CRCL, SNDK, RIVN, USAR, TSM, BABA, CRWV, DKNG, HIMS, COST, LLY
- **韓国株**: SKHX, SMSN, HYUNDAI, EWY
- **日本/地域**: EWJ
- **Pre-IPO**: CBRS

### 1.4 標準 crypto perp の Oracle 仕様 (参考)
- BTC/ETH等: Binance, OKX, Bybit, Kraken, Kucoin, Gate IO, MEXC + HL spot mid の **加重中央値** (重み 3,2,2,1,1,1,1,1)
- 検証者(validators)が3秒ごとに発行 → ステーク加重中央値が最終Oracle
- HYPE等HL主体銘柄は外部ソース除外、BTC等はHL spot除外

## 2. Mark Price と Oracle Price の関係 (核心)

### 2.1 標準HL crypto perpの Mark Price 計算
3つの中央値:
1. Oracle価格 + 150秒EMA(HL mid - oracle)
2. HL内部のbest bid/ask/last trade の中央値
3. Binance/OKX/Bybit/Gate IO/MEXC の perp mid 加重中央値 (3:2:2:1:1)

### 2.2 EMA計算式
```
ema = numerator / denominator
numerator → numerator * exp(-t / 2.5min) + sample * t
denominator → denominator * exp(-t / 2.5min) + t
```

### 2.3 Discovery Bounds
- Mark price は参照価格±2%〜±10%でクランプ (アセットごと)
- 1更新あたり ±50 bps クランプ (大きな jump 防止)

## 3. 米株 perp の Oracle 切替メカニズム (戦略設計の核)

### 3.1 active session (US equities open)
- 外部データ (EMM6 / SPX index等) を直接Oracleとして使用
- リレイヤーが3秒ごと配信
- → Hyperliquid SP500 perp は CME ES (EMM6) を実質的にトラッキング

### 3.2 closure (週末・夜間・祝日)
- 外部データ unavailable → **内部EMA機構が起動**
- 公式: $S_t = \beta_t S_{t^-} + (1-\beta_t) x_t$
  - $\beta_t = \exp(-\Delta t^*/\tau)$
  - $x_t = S_{t^-} + IPD_t$ (impact price difference)
  - $\tau = 30$分 (equity perpetuals)
  - $\Delta t^* = \min(\Delta t, c\tau)$, $c = 0.1$ (1更新あたり最大~9.5%)
- IPD = $\max(P_{impactBid} - S, 0) - \max(S - P_{impactAsk}, 0)$
- → **Hyperliquid内部の板から価格が形成される時間帯がある**
- 外部データ復活時: 次のtickで externally-derived spot に戻る

### 3.3 戦略的含意 (重要)
**closure中:**
- Hyperliquid SP500 perp の価格は **HL内部の板** に依存して動く
- 外部参照がない時間帯 = 「Hyperliquid独自の価格発見」が成立する時間帯
- Geminiの心配 (CME強制連動でアルファ無し) は **active sessionでは正しい**が、**closure中は反対**
- 月曜寄り(active session開始)で外部Oracleに復帰 → ここでギャップが生じる可能性
- IPD (impact price difference) の計算は HL の板厚に依存 → 板薄い銘柄 (XYZ100, 個別株) は内部EMAで歪みやすい

## 4. Funding の現実コスト (検証済み)

### 4.1 標準 crypto perp
- Premium = impact_price_difference / oracle_price
- IPD = max(impactBid - oracle, 0) - max(oracle - impactAsk, 0)
- F = avg(premium) + clamp(interest - premium, ±0.0005)
- interest = 0.01%/8h = 11.6% APR (default)
- 8時間レートを1時間ごとに分割支払い
- 全体キャップ: 4%/hour

### 4.2 XYZ equity perp (米株 perp)
- F_xyz = **0.5** × [Premium + clamp(r - Premium, ±0.0005)]
- 0.5x multiplier により default funding ~5.5% APR (SOFR + 1-2%)
- 1時間ごと支払い
- Cap: ±4%/hour
- 「weekend price discovery を dampen する」と明記
- closure中もfunding は active

### 4.3 ペアトレード時のfunding二重支払いリスク (Geminiv2指摘)
- 例: SP500 long + XYZ100 short
- 両方のpremium index 符号によっては両方支払い側になる
- 0.5x multiplier のおかげで標準perp比はマシだが、それでも年率最大数%レベル

## 5. API 仕様

### 5.1 取得できるエンドポイント (公式 SDK 確認)
- Info API (REST):
  - `meta` / `metaAndAssetCtxs`: 全銘柄の funding/mark/oracle
  - `l2Book`: 板スナップショット
  - `candleSnapshot`: 過去ローソク
  - `userFunding` / `fundingHistory`: funding履歴
  - `recentTrades`: 直近約定
- Exchange API (REST + EIP-712 署名): 注文・建玉操作
- WebSocket: l2book / trades / candle / orderUpdates / fundings 等

### 5.2 注意点 (Gemini指摘反映で要確認)
- WS切断時のリプレイ仕様 (最終ミラー時刻からのbackfill REST)
- rate limit 詳細 (公開だがHL gitbook で確認要)
- ヒストリカル lookback 深さ (candleは数年分取れる、tick は限定的)

### 5.3 過去のSurrealDB知見との関連
- 2026-04-04 「hyperliquid-api-spec-market-maker」で Rust SDK・EIP-712 既確認
- 米株 perp は HIP-3 deployer 経由でも同一APIから読める

## 6. Geminiの懸念に対する回答

| Gemini懸念 | Phase 0 回答 |
|---|---|
| HL SP500 が CME ES をそのまま引っ張っているなら独自価格発見は無い | **半分正しい**: active session中はEMM6 (CME e-mini)直結。**しかしclosure中はHL内部EMA** → 週末は独自価格発見がある |
| HL内ペアトレード (SP500 vs XYZ100) は擬似cointegration | active session中は両方外部直結なので相関はTradFiの相関と同等。closure中は両方とも内部EMAで動く → 相関は崩れる可能性 |
| Funding二重支払いリスク | 確実に存在。ただし0.5x multiplierで影響は標準perp比半分 |
| 板厚不足 | XYZ100や個別株は明らかに薄い。SP500ですらBTC perp比1-2桁低い (要実測) |
| 週末は独自価格発見ない | **誤り**: 週末こそHL内部EMAで独自価格発見が起きる |

## 7. 戦略設計への含意 (v3 設計ドラフトに反映すべき点)

### 7.1 「TradFi vs Crypto」テーマが復活する余地
- 週末・closure中はHL内部で価格が形成される → **これが本当のアルファ源**
- active session中は CME に強制連動 → アルファ薄い
- → **戦略は週末・closureに集中**するのが筋

### 7.2 ペアトレード設計の見直し
- SP500-perp vs XYZ100-perp: active session中は外部相関と同じ → アルファ薄い
- closure中は両方内部EMAで独立に動く → スプレッドの divergence/convergence が出やすい
- ただしXYZ100は板薄でIPDの動きが大きい → 期待値は要実測

### 7.3 Capacity 制約 (Gemini指摘反映)
- SP500 ですらBTC perp比1-2桁低い流動性
- 個別株はさらに薄い
- 自分のサイズが板に対してN%超えると自家中毒 (impact price difference を自分で動かす)
- Phase 1 で板厚プロファイルを実測必須

### 7.4 Phase 0 でわからなかったこと (Phase 1 で実測すべき)
- 実際の出来高 (各銘柄、時間帯別)
- 実際のスプレッド (時間帯別、closure中の挙動)
- closure中のIPDから生じる価格ドリフト分布 (実データで)
- monday open gap の分布
- Trade[XYZ] のリレイヤー停止頻度・遅延

