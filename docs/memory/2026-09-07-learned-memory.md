# peri — learned memory

Exported from the live ledger on 2026-09-07 09:07Z. Two halves, the same
split the running bot sees each cycle: lessons the analyst wrote for itself,
and a measured record computed from every closed trade. The measured half
cannot be hallucinated. It is recomputed from the ledger, not stored prose.

## Lessons (42)

- **2026-08-31** (operator, pinned) An unfilled resting entry can now be REPLACED: opening again on a market where you already hold one withdraws the old order and places the new one. Use it when the level is running away from a thesis you still believe, instead of watching the order expire — on 2026-08-31 the xyz:CL entry died unfilled at 08:17 while crude went on to 86.50.
- **2026-08-31** (operator, PUMP, pinned) Being early to a caller's idea is not an edge. On 2026-08-31 PUMP was entered at 0.004418, 29 minutes before the caller entered the same trade at ~0.00440 — the extra time bought a WORSE fill, not a better one, because the level had not finished coming to him. His call is information about direction, never a reason to pay up ahead of it.
- **2026-08-31** (operator, pinned) A resting entry only pays if the market comes back to it. On 2026-08-31 three in a row were priced 1.5-1.9% under the mark on TRENDING tape and none filled: xyz:CL at 84.50 missed the session low of 84.55 by five cents and ran to 86.50, the 84.10 replacement never came close, and xyz:BRENTOIL at 89.45 sat 1.91% below a market whose 8h low was 89.55. Size the discount to the tape: on a market trend
- **2026-08-31** (operator, pinned) Only US single-name equities are refused at the weekend and around the cash open/close. Never generalise that to the whole xyz dex: doing so cost a tradeable oil move on 2026-08-30.
- **2026-08-31** (operator, pinned) Foreign builder-dex equities trade their home exchange: SKHX/SMSN/SKHY/HYUNDAI on the KRX (09:00-15:30 KST), KIOXIA/SOFTBANK/IBIDEN on the TSE (with a lunch break), TENCENT on the HKEX. SKHX is the largest market on the dex and is live while the US sleeps.
- **2026-08-31** (operator, pinned) Weekend oil perps (xyz:CL, xyz:BRENTOIL) track live futures, unlike equity synthetics with no cash market — a confirmed supply catalyst such as a Hormuz or Kharg Island escalation is genuinely tradeable there while US stocks are shut.
- **2026-08-31** (operator, pinned) Builder-dex markets keep their OWN underlying's hours, not New York's. Commodities and index futures (CL, BRENTOIL, GOLD, SILVER, NATGAS, COPPER, SP500, XYZ100) trade Globex: Sunday 18:00 ET through Friday 17:00 ET with a one-hour halt each day at 17:00. They are live all Sunday evening and all night on a weekday.
- **2026-08-29** (operator, pinned) After a losing day the correct size is smaller or zero. Trying to win it back in one trade is how the account dies.
- **2026-08-29** (operator, pinned) A round trip costs ~0.15% of notional (less on a resting maker entry). On a small account that is a fifth of the risk budget, so only setups worth 2.5R or more are worth taking.
- **2026-08-29** (operator, pinned) A stop tighter than 2% or 4x ATR15m is noise, not risk: it gets hit before the thesis has a chance.
- **2026-08-29** (operator, pinned) Never open a builder-dex position near the US open or close: the CRWD long entered 2 minutes before the bell and was stopped 6 minutes later.
- **2026-08-29** (operator, pinned) Chasing an extended move is how I lose: on 2026-08-28 six of six entries at the edge of the range stopped out inside an hour. Wait for the pullback and rest a limit.
- **2026-09-03** (analyst, ETH) A major short justified by fresh momentum is invalidated the moment its r1h/r4h flip positive while the rest of the complex goes broadly bid — exit at the small loss then, not at the structural stop. The stop is disaster protection, not the planned exit; waiting for it turns a -0.2R scratch into a -1R hit on the same dead thesis.
- **2026-09-02** (analyst, HYPE) A market banned in memory must be treated as untradeable in the moment, not re-evaluated on a hunch: today's HYPE long was taken despite an explicit 0-for-2 ban and lost again (now 0-for-3, ~-$5.60). Re-evaluating a banned setup is how the same losing trade gets entered a third time; the +37pp smart-money divergence never once produced momentum. HYPE longs stay banned until one actually wins.
- **2026-09-02** (analyst, io:GPRO) io:GPRO squeeze days (+45%, funding -191% APR = shorts paying to be there) carry ATR15m ~5.5%, so the 4x-ATR rail forces a ~22% stop; at 5x max leverage and the $3 net-TP floor the only viable shape is riding momentum from a pullback below mid-range. At 0.6+ of range after a +45% day with no verifiable catalyst it is a chase — skip, and never let a 5m trigger override the daily entry cap.
- **2026-09-02** (analyst) Three losses in one day (HYPE long, ETH short, CBRS short) all shared the same flaw: no dated catalyst that could resolve inside the 3h dead-money window. Before any entry answer 'what moves this in the next 3 hours?' — if the answer is nothing, or a macro print 38h away like NFP, skip the trade no matter how good the level or divergence looks.
- **2026-09-02** (analyst, HYPE) HYPE longs are 0-for-2 (-$3.59): both were mid-range entries without a fresh catalyst that decayed and were closed inside the 3h window. A positive smart-money divergence (+37pp) is positioning evidence, not momentum — treat HYPE as short-the-bounce only or avoid until a long actually works.
- **2026-09-02** (analyst, xyz:HOOD) A trigger that arrives after the daily entry cap (3/3) is spent is a forced rest, not a missed trade — especially on a US single name after cash close, where the entry is refused on three independent rails (session shut, chase-refused at range-pos 1.00, longs paying +90%+ funding). Log the breakout level and reassess at the next session; never hunt for a workaround market.
- **2026-09-02** (analyst) On catalyst-void days — when no HIGH-impact event lands inside the 3h dead-money window (NFP was 39h out) — every entry taken lost: HYPE dip-buy, CBRS fade against a flipping bias, and an ETH short with no continuation due, 0/3 for -$2.72 of chop. Cap such days at one trade maximum; extra entries only donate fees to range noise.
- **2026-09-02** (analyst, ETH) The 3h dead-money rule is the binding constraint on major shorts: the 2391 short filled perfectly on the retest yet sat at +0.01R at 2h55m because the continuation it needed (NFP) was 40h away. Only fade bounces on majors when the continuation catalyst or fresh lower-lows momentum falls INSIDE the 3h window — a correct level with the wrong clock still closes flat and pays fees for nothing.
- **2026-09-02** (analyst, ETH) My record is sharply asymmetric by side: shorts ~67% win (avg +0.29R) while longs ~33% win (avg -0.30R). In this risk-off regime my edge is fading bounces of breaking-down majors, not catching their falling knives. When BTC/ETH are pressing lower, default to shorting retests; only go long on a confirmed base or a dated catalyst, never on a dip alone.
- **2026-09-02** (analyst, xyz:HOOD) US single-name breakout triggers at range-pos 1.00 in the final minutes of the cash session are untradeable by construction: entries are refused within 30m of the close and the overnight gap would make any stop unreliable anyway. Log the level and reassess at the next session, never chase the print.
- **2026-09-02** (analyst) After 21 trades the split is unambiguous: shorts 67% win, +0.29R avg (6 trades) vs longs 33% win, -0.30R avg (15 trades). My edge is fading crowded longs on weak tape, not dip-buying. When entry slots or the daily cap are scarce, reserve them for short setups and skip marginal longs entirely.
- **2026-09-02** (analyst) My longs win 33% (-$8.19 over 15 trades, avg -0.30R) while my shorts win 67% (+$0.94 over 6, avg +0.29R): on weak tapes my edge is fading strength, not dip-buying. When a long and a short setup look comparable, take the short, and demand materially stronger evidence before any long entry.
- **2026-09-02** (analyst, xyz:CBRS) On pre-IPO price-discovery perps, a smart-vs-crowd divergence snapshot can flip completely within the hour: CBRS went from -49pp (smart short) to +10pp (smart long) while price made new highs. The fade thesis died with that flip — re-check live bias before trusting a divergence, and treat a full positioning reversal plus new highs as immediate invalidation, not a level to defend with a stop.
- **2026-09-02** (analyst) My ledger's asymmetry is persistent and causal: shorts are 80% win / +0.38R avg while longs are 33% / -0.30R. When the entry slot is scarce or the tape is ambiguous, give the slot to the short setup with a smart-money divergence against a crowded long, and treat long dip-bids as the exception that needs an extra catalyst, not the default.
- **2026-09-02** (analyst, xyz:BRENTOIL) War-premium pullback longs in crude must be withdrawn the moment de-escalation tape appears (rally-stalling headlines, supply resuming, normal transit volumes): the geopolitical premium bleeds faster than it built, and an unfilled dip bid left in place becomes a falling-knife catch instead of a trend continuation entry.
- **2026-09-02** (analyst, xyz:DELL) A fresh earnings beat on a high-ATR single name (DELL ATR15m ~2.3%) forces a >=9% stop, so even a 2R target needs an ~18% continuation that will not land inside the 3h dead-money window — a dated catalyst does not fix the geometry; skip the day-one reaction and wait for a lower-volatility base instead of chasing +5% at mid-range.
- **2026-09-02** (analyst) Record audit: my longs are 15 trades, 33% win, -$8.19 (mostly dip-buying high-beta in risk-off), while my shorts are 5 trades, 80% win, +$1.35. On a red tape with negative smart-money divergence, default to resting shorts into bounces; take longs only when they carry a dated catalyst AND a structural tailwind like negative funding paying me to hold (the BRENTOIL Hormuz retest shape).
- **2026-09-02** (analyst, xyz:PLTR) When a single name breaks its 24h low hard while its sector peer holds up (PLTR -6% vs NVDA +3%), the break is idiosyncratic flow, not a sector dip — with no visible catalyst there is no reason to buy the low; the break itself is evidence the name is falling for a reason I don't see. Wait for a base to form or a dated catalyst.
- **2026-09-02** (analyst, io:GPRO) [io:GPRO] Unexplained mid-range pops on micro-OI io: synthetics (OI <$1M, ATR15m ~2.2%) are untradeable: the mandatory 4x-ATR stop (~8.8%) forces an ~18% TP to clear the 2R net floor, and such a thin book never delivers that inside the hold window — especially when smart money is -20pp less long than the crowd. Skip the trigger and keep the resting orders that already express the thesis.
- **2026-09-02** (analyst) A price trigger on a market outside CANDIDATES (e.g. a 3x ETF like SOXL) is context, not an entry: by the time you route around it to a proxy, the proxy is either chase-refused at the top of its range (QCOM at 0.98) or its home exchange is closed (KIOXIA on TSE hours). Do not force the second-best name — keep the resting orders that already express the thesis and skip the cycle.
- **2026-09-02** (analyst) Record asymmetry is consistent: shorts are 5/5-ish (80% win, +0.38R avg, 191m median hold) while longs are 33% win, -0.30R avg, 55m hold — the longs are failed dip-buys on red tape. My edge is fading bounces on risk-off days and letting the short work, not scalping long dips; treat any long entry while majors sit at the bottom of their range as low-conviction by default.
- **2026-09-02** (analyst) An operator pause is total and does not expire on its own: while it is active every open is refused regardless of setup quality (SOL and ETH were both bounced 62m apart on the same pause). While paused, emit zero open actions and manage only what is open — re-submitting into the rail just wastes the cycle.
- **2026-09-02** (analyst) Telegram position cards that already show an EXIT price are completed trades posted after the fact — the poster is already out and the move is spent. There is nothing to mirror; treat them as tape history, and only fresh levels or charts expressing unexecuted intent count as signals.
- **2026-09-02** (analyst, HYPE) HYPE longs entered while BTC/ETH are breaking down at the bottom of their 24h ranges have gone 0/2 (-$3.59): HYPE's beta means it does not lead the rebound, it follows it — short the bounce or skip it entirely, never dip-buy a high-beta name on a risk-off tape.
- **2026-09-02** (analyst) Record asymmetry: longs are 36% win at -0.27R avg while shorts are 80% win at +0.38R. On risk-off tapes (yields rising, majors at the bottom of their 24h ranges), do not dip-buy caller support zones with longs — keep resting shorts into bounces instead, and accept that if no bounce comes the entries expire free; sitting flat beats catching the knife.
- **2026-08-31** (analyst, xyz:CL) Official verbal intervention (Treasury Secretary saying oil 'is going to come down') plus a supply-restoration headline is the fade signal for a geopolitical oil spike: if the 4h base under a spike-long breaks while that catalyst is live, close at market instead of waiting for the wider bracket stop — the stop is priced for a continuation that the headlines have just cancelled.
- **2026-08-31** (analyst, SOL) Mirroring a caller's tight-stop scalp only works if the fill is AT or better than his entry; entering even ~0.2% above a 0.37% stop-distance call cuts RR from ~2.1 to <1 once the venue mounts its own wider stop. Close at breakeven rather than ride the degraded structure — the caller's edge died with the fill, not with the direction.
- **2026-08-31** (analyst) A caller screenshot that shows an exit price and PnL is a recap of a completed trade, not a fresh entry — do not mirror it; trade only calls with a live entry.
- **2026-08-31** (analyst, PUMP) A caller's 'Risky' scalp on a high-beta alt is conditional on its stated trigger (e.g. BTC/SOL breakout); if the caller is stopped out before the trigger fires, exit at once even when your own bracket is wider — holding converts his scalp into a hope position on a falling tape and the wider stop only delays the same exit at a worse price.
- **2026-08-31** (analyst, xyz:BRENTOIL) A resting entry placed before another position consumes the account's margin can become silently unfundable at fill (available_margin < notional/maxLev). Before counting on a resting fill, check free margin against its implied posting requirement; if it cannot fund, treat it as a zero-cost option that may self-rescue if the other position closes, and do not build plans around it.

## Measured record

All 27 closed trades: 12 winners, 44% win rate, $-24.98 net.

### By entry style

| bucket | n | win rate | net $ | avg R | median hold |
|---|---:|---:|---:|---:|---:|
| market (unknown) | 20 | 50% | -22.33 | -0.14 | 133m |
| resting | 7 | 29% | -2.65 | -0.14 | 87m |

### By side

| bucket | n | win rate | net $ | avg R | median hold |
|---|---:|---:|---:|---:|---:|
| long | 18 | 39% | -30.67 | -0.31 | 97m |
| short | 9 | 56% | +5.69 | +0.20 | 171m |

### by range position

| bucket | n | win rate | net $ | avg R | median hold |
|---|---:|---:|---:|---:|---:|
| mid range | 4 | 25% | -3.11 | -0.27 | 181m |
| top of range (>=0.80) | 3 | 33% | +0.46 | +0.04 | 53m |

### by close reason

| bucket | n | win rate | net $ | avg R | median hold |
|---|---:|---:|---:|---:|---:|
| analyst | 11 | 27% | -8.80 | -0.22 | 66m |
| external | 5 | 80% | -17.31 | +0.05 | 573m |
| operator | 1 | 0% | -0.00 | -0.00 | 53m |
| sl | 8 | 38% | -5.42 | -0.48 | 171m |
| stop 1.413 (+1R ratchet) filled @ 1.4129 | 1 | 100% | +3.08 | +0.80 | 460m |
| tp | 1 | 100% | +3.48 | +1.31 | 0m |

### worst markets

| bucket | n | win rate | net $ | avg R | median hold |
|---|---:|---:|---:|---:|---:|
| BTC | 5 | 40% | -28.49 | -0.89 | 133m |
| PUMP | 1 | 0% | -3.96 | -0.35 | 55m |
| HYPE | 2 | 0% | -3.59 | -0.99 | 186m |

### best markets

| bucket | n | win rate | net $ | avg R | median hold |
|---|---:|---:|---:|---:|---:|
| SOL | 4 | 75% | +6.03 | +0.38 | 338m |
| ETH | 3 | 33% | +4.75 | +0.03 | 87m |
| XRP | 1 | 100% | +3.08 | +0.80 | 460m |

## Every closed trade (27)

| closed | market | side | entry | exit | net $ | style | range pos | reason |
|---|---|---|---:|---:|---:|---|---:|---|
| 08-27 16:32Z | xyz:MRNA | short | 145.5 | 141.5 | +1.07 | - | - | sl |
| 08-27 16:52Z | xyz:KIOXIA | short | 333.76 | 321.95 | +1.30 | - | - | analyst |
| 08-27 18:26Z | xyz:NVDA | long | 224.44 | 226.84 | +0.58 | - | - | sl |
| 08-27 20:02Z | SOL | long | 105.41 | 109.13 | +2.14 | - | - | external |
| 08-27 20:05Z | BTC | long | 80464 | 79870 | -0.94 | - | - | analyst |
| 08-28 01:05Z | xyz:MRNA | short | 142.93 | 142.44 | +0.25 | - | - | sl |
| 08-28 02:52Z | BTC | long | 80707 | 79735 | -1.57 | - | - | sl |
| 08-28 08:49Z | SOL | long | 106.17 | 106 | -0.26 | - | - | sl |
| 08-28 13:20Z | xyz:MRVL | short | 221.09 | 224.4 | -1.43 | - | - | sl |
| 08-28 13:35Z | xyz:CRWD | long | 228.17 | 223.89 | -2.67 | - | - | sl |
| 08-28 14:17Z | HYPE | long | 83.741 | 82.585 | -1.38 | - | - | sl |
| 08-28 15:21Z | xyz:PALLADIUM | long | 1468.4 | 1454 | -0.69 | - | - | analyst |
| 08-29 03:12Z | BTC | short | 77720 | 77572 | +0.16 | - | - | external |
| 08-30 22:07Z | BTC | long | 78380 | 78664 | +0.30 | resting | 0.62 | analyst |
| 08-31 10:12Z | PUMP | long | 0.004418 | 0.004337 | -3.96 | - | - | analyst |
| 08-31 14:14Z | xyz:CL | long | 85.6 | 85.659 | -0.00 | resting | 0.84 | operator |
| 08-31 14:40Z | SOL | long | 102.58 | 102.99 | +0.67 | - | - | analyst |
| 08-31 14:55Z | xyz:CL | long | 85.59 | 85.362 | -1.67 | - | - | analyst |
| 09-02 06:29Z | SOL | long | 103.12 | 103.75 | +3.48 | - | - | tp |
| 09-02 11:08Z | HYPE | long | 82.2 | 80.917 | -2.21 | resting | 0.60 | analyst |
| 09-02 19:18Z | xyz:CBRS | short | 183.97 | 184.36 | -0.41 | resting | 0.82 | analyst |
| 09-02 20:49Z | ETH | short | 2391 | 2392.8 | -0.24 | resting | 0.23 | analyst |
| 09-03 02:43Z | ETH | short | 2390.5 | 2404.2 | -0.97 | resting | 0.33 | analyst |
| 09-06 09:47Z | BTC | long | 77783 | 78675 | -26.44 | - | - | external |
| 09-06 09:47Z | ETH | short | 2519 | 2506.4 | +5.96 | - | - | external |
| 09-06 09:47Z | xyz:KIOXIA | long | 322 | 325.24 | +0.88 | resting | 0.93 | external |
| 09-06 22:14Z | XRP | long | 1.404 | 1.4129 | +3.08 | - | - | stop 1.413 (+1R ratchet) filled @ 1.4129 |

## Rationale the analyst recorded, trade by trade

- **2026-08-27 xyz:MRNA short** (+1.07) operator bounce-short filled 145.5: continuation of Aug-20 -25% valuation-reset unwind. Stop 147.5 (your tighten). TP restored at 137.5.
- **2026-08-27 xyz:KIOXIA short** (+1.30) operator carry-short 11:36Z @333.76: longs paid ~100% APR (short collects); memory blow-off fade. WORKING: mark ~324. Stop 331.5 (your tighten — good, locks profit). TP restored at 315 (pre-blowoff shelf).
- **2026-08-27 xyz:NVDA long** (+0.58) Confirmed blockbuster earnings (rev beat, +6% reaction) igniting sector-wide AI bid; NVDA basing at 0.89 of range with positive 1h momentum and deepest liquidity on the board. Momentum-continuation long with a 2.5x ATR15m stop.
- **2026-08-27 SOL long** (+2.14) Confirmed structural catalyst (Schwab to add SOL/AVAX/LINK trading, two independent headlines <25m old) with price +9% and still making higher lows (r1h +1.0%, r4h +0.9%) on a broadly risk-on AI-led tape. Buying the continuation of a distribution-driven repricing, sized modestly given 0.90 range position and Jackson Hole later this week.
- **2026-08-27 BTC long** (-0.94) Fresh confirmed US-demand catalyst: Coinbase premium positive for the first time in 40 days alongside NVDA-earnings risk-on spillover and headlines targeting $81K. BTC basing at 0.86 of range with positive 4h momentum and deepest liquidity; modest 3x sizing given Jackson Hole keynote risk and +9.7% funding.
- **2026-08-28 xyz:MRNA short** (+0.25) Only red candidate on a melt-up day, confirming idiosyncratic supply after its late spike; my prior short (145.5 -> 141.5) worked and the operator-anchored continuation target is 137.5. The intraday bounce (r4h +1.0%) lets me enter on strength with a defined supply cap above 144.9; ~2.7R structure into a measured target below today's lows.
- **2026-08-28 BTC long** (-1.57) BTC is leading a confirmed risk-on breakout at a two-month high with squeezed shorts, dovish-Fed-chair speculation, and gold near records; alts (SOL, MSTR, XRP) all confirm breadth. Entering continuation with a stop under the 79.9k demand shelf that already absorbed one pullback, targeting the next leg to 83.6k (~3.1R).
- **2026-08-28 SOL long** (-0.26) SOL +4.9% on the day after breaking resistance; chat/caller sentiment bullish ('market is hot', SOL breakout calls) and I already won this exact structure long 105.4->109.1. Buying the 4h pullback toward 106 with a structural stop under the 104.5-104.75 swing/short zone, targeting continuation above the 109.13 high (~3.2R).
- **2026-08-28 xyz:MRVL short** (-1.43) Confirmed negative catalyst: Q2 outlook underwhelmed on lumpy custom-AI/data-center demand (TD Cowen flags 'lack of upside'), stock is -13% on the day, sitting at 0.05 of its 24h range with r1h still -1.4% — post-earnings drift continuation, echoed sector-wide by SNDK -7.4% and MU -5.9%. Best liquidity among the broken names ($53M vol). Entering short at 221.04 with a stop above the bounce zone and a ~3.2R target into the low-210s.
- **2026-08-28 xyz:CRWD long** (-2.67) CrowdStrike delivered what CEO called the best quarter in its history with a beat-and-raise and AI-driven security demand, and the stock is holding near day highs (0.88 range pos) two sessions on with deeply negative funding (-53.6% APR) meaning shorts are crowded and paying longs — squeeze fuel. Entering the small hourly pullback near the consolidation shelf with a stop under it and a ~3.7R continuation target; structure projects ~$4.4 net at TP after fees, inside the policy target.
- **2026-08-28 HYPE long** (-1.38) Caller flagged 'hype is pumping' and search confirms a live HYPE-specific catalyst chain: AQAv2 buyback activation, projected ~18% revenue boost, treasury-company accumulation, and whale longs, with press targets near $90. Tape agrees: +1.2% 1h, +1.5% 4h, range-pos only 0.37 so the move is not extended, and OI/volume are deep. Long the continuation with stop under the 4h consolidation and TP below the 88 resistance shelf (~3.6R).
- **2026-08-28 xyz:PALLADIUM long** (-0.69) Confirmed supply catalyst (US moving to ~109% prohibitive countervailing duties on Russian palladium in a market where Russia is the dominant producer) plus the strongest tape on the board: +10% day, 1.00 range position, positive 1h/4h momentum, thin $3.3M OI leaving shorts trapped. Buying the squeeze continuation with a stop under the 1430 consolidation and a 2.8R target at 1546; isolated margin contains risk in this low-liquidity builder market.
- **2026-08-29 BTC short** (+0.16) operator: post-Warsh risk-off, BTC failed 2-month breakout, lower highs off 76.8k low, longs paying funding; target +10 net
- **2026-08-30 BTC long** (+0.30) Confirmed Saylor/Strategy resume-buying catalyst with shorts already squeezed (66% of $9.7B liquidations) gives a real bid; instead of taking 0.62 of range now, rest a maker long near the 24h midpoint (~0.42 of range) so the entry itself improves RR. Stop sits below the 24h low at >2% width; TP is 2.5R back through the 80.7k swing high. Funding +10.9% APR is a trivial carry cost against the expected move.
- **2026-08-31 PUMP long** (-3.96) operator manual: PUMP bounce off the 24h low, stop under 0.004182, target the 24h high
- **2026-08-31 xyz:CL long** (-0.00) Replacing my resting bid (expires in 77m) to keep the level alive while the escalation headlines persist — Trump vowing to 'hit them hard' 16m ago keeps the supply premium live. My measured lesson prescribes exactly this: geopolitical oil spikes fade from ~0.9 toward ~0.6 of range within hours; the trade is a maker bid below the spike, refreshed while headlines run. Mark 86.18 is already drifting down (r1h -0.27%) toward the 85.6 bid (~0.7 of the 24h range, not a chase). Smart money +25pp long CL. Stop 83.8 is 2.1% / ~5x ATR15m below entry, under the pre-spike shelf; net at TP ~$5.8 after fees. 10x isolated because a 2.1% stop fails the 20x isolated rail.
- **2026-08-31 SOL long** (+0.67) mirror the caller's call 14:17Z: SOL long entry 102.37 SL 102 TP 103.15; range-pos 0.33, taken at market 0.14% above his level
- **2026-08-31 xyz:CL long** (-1.67) shelf long: 4h base 85.44-85.62 held four times, range-pos 0.71 not chased, stop under the shelf
- **2026-09-02 SOL long** (+3.48) mirror the caller's call 15:00Z: SOL long, HIS level 103.12 SL 102.70 TP 103.75 — rested, not chased
- **2026-09-02 HYPE long** (-2.21) Refresh my resting pullback bid before it expires in 68m — thesis unchanged and price is drifting toward it (r1h -0.42%). HYPE carries the strongest positioning signal on the board: smart wallets +50pp more long than the crowd, with mark 82.9 at 0.60 of the 24h range on a -1.3% day. Entry 82.2 gives a 2.25% stop (above the 2%/4x-ATR floor) and 2.54R to 86.9, net TP well above the $3 floor; the tape is soft enough that the market may come to me, and if it doesn't the order expires free.
- **2026-09-02 xyz:CBRS short** (-0.41) Largest smart-money divergence on the board (-49pp: profitable wallets 31% long vs crowd 80% long, 701 long wallets to 110 short) on a pre-IPO price-discovery perp sitting at 0.82 of its 24h range after a +4.5% run, with r4h already -0.66% and funding +5.5% APR paying me to short the crowded long. Resting 0.1% above the mark to let any micro-pop hand me a better fill instead of taking it flat; 2.46% stop clears the 4x-ATR (2.24%) rail and 2.56R to 171.5 nets ~$6.5 after fees at the engine's ~5% risk size. My measured record: shorts 80% win / +0.38R avg vs longs 33% / -0.30R — this is the side of the book I actually make money on, and it hedges the NVDA long and ETH short macro-wise.
- **2026-09-02 ETH short** (-0.24) Replacing my 2401 resting short (expires in 84m) because the tape is running away from that level: ETH is -2.4% on the day at range-pos 0.23 and still pressing lower while profitable wallets are 16pp less long than the crowd and funding pays me to be short. Repricing to 2391 — a 0.7% pop above the mark — keeps the fade alive at a level a dead-cat retest can actually reach, with a 2.68% stop (5.3x ATR15m) and 2.5R to 2231, ~$6.9 net of fees.
- **2026-09-03 ETH short** (-0.97) ETH is pressing to fresh lower lows (r1h -0.47%, r4h -0.53, day -1.14%) while BTC/SOL are flat — idiosyncratic weakness, not broad tape. Smart wallets are 40% long vs crowd 65% (-26pp): the profitable cohort is not positioned for a bounce, and +10.9% funding means I am paid to short. This is exactly the recorded edge — fading retests of breaking-down majors with fresh momentum inside the 3h window (NFP is 35h out, outside it, so momentum must carry the trade, and it is carrying now). Rest a maker short at a modest 0.4% retest rather than sell the 0.33 range-pos print; stop 2.07% above entry clears the 4x-ATR rail, TP 2.2R nets ~$5.5 after fees.
- **2026-09-06 BTC long** (-26.44) 48h swing: 14d uptrend +12.4% intact, pullback to the 7d shelf 76234-76831 tested 5x, stop below it, target the 14d high, held through Fri 12:30Z NFP
- **2026-09-06 ETH short** (+5.96) smart money -22pp short vs crowd 64pc long, funding +11pc APR, rallied to 0.84 of 24h range while 7d net -1pc; resting at the 24h high, stop above the 7d high
- **2026-09-06 xyz:KIOXIA long** (+0.88) KIOXIA broke its 24h high (327.9) on the TSE open with a real two-legged catalyst: the AI/memory spot squeeze (BofA +10-20% Sept forecast) plus the Apple event 6 days out, where Kioxia leads NAND supply. Smart wallets are 75% long vs 31% crowd (+44pp) and shorts pay -45.9% APR, so dips are being bought and fuel is on the short side. At 0.93 of range the market price is a refused chase, so rest a maker limit ~1.7% under the mark to catch the post-breakout retest; stop sits under the gap-origin/day-open area, TP at 2.1R into squeeze continuation.
- **2026-09-06 XRP long** (+3.08) operator goal: double the account. smart +16pp; shelf validated at 1.4030; rested ON it, stop 2x ATR under, target the 7d high. peri is STOPPED and DISABLED — do not replace this order

