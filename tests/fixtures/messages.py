# Message shapes the feed and the analyst must handle. SYNTHETIC: each mirrors a
# format seen in a real caller group -- an entry with take-profit and stop on
# their own lines, a two-target variant, a bare directional call, an in-flight
# management instruction, an exit recap, and ordinary chatter -- without
# reproducing anyone's actual words. Format fidelity is the point; the wording
# is invented. Extend by adding a NEW SHAPE, not another paraphrase.
ENTRIES = [
    "SOL Long\n\nTP: im taking 108, safer would be 105\n\nSL: wide, im not setting one",
    "ARB LONG\n\nTP: 0.21\nsafer TP: 0.198\n\nSL: didn't place any",
    "AVAX LONG\n\nTP: 9.15\nSL: put it where you're comfortable",
    "SILVER SHORT\n\nTP: 63.80\nSL: 66.20",
    "BTC SHORT\n\neasy on the size here, expect chop",
    "DOGE Short (Down)\nsmall lev",
    "Going long UNI here",
    "LTC Short",
]
MANAGES = [
    "looking good, take 50% off now\nand pull SL up to 2612",
    "half off here\nsl to entry",
    "running well, book half",
    "pulled my SL to +4%",
]
EXITS = [
    "hit full tp, gg",
    "closed the rest",
    "out of the LTC short. looks strong now",
    "Flat on this one.",
    "safer target filled",
]
NOISE = [
    "gm",
    "anyone still in a trade?",
    "ETH long??",
    "Ok to enter here ?",
    "shorts only until this level breaks, tight stops",
    "\U0001F4CA WEEKLY RECAP\n1 SILVER SHORT WIN\n2 ETH LONG LOSS",
    "**JUST IN:** Central bank holds rates, signals one cut this year.",
]
