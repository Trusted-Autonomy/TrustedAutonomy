#!/usr/bin/env python3
"""Reference domain-action adapter for TA v0.17.5.3 -- paper trading only.

Handles the "trade.execute" verb declared in this directory's plugin.toml
(`capabilities = ["verb:trade.execute"]`). No live brokerage API is called
anywhere in this file -- every "execute" is a simulated fill, and every
"risk_score" is computed from the request payload alone. This is what makes
it safe to use as TA's own reference implementation and test fixture (see
PLAN.md v0.17.5.3 item 5: "no live external API dependency in TA's own test
suite").

Wire protocol (one JSON line in on stdin, one JSON line out on stdout, per
`ta_plugin::envelope`/`ta_plugin::transport::call_json` -- the same
transport VCS and release plugins use):

    in:  {"method": "risk_score", "params": {"verb": "trade.execute", "payload": {...}}}
    out: {"ok": true, "result": {"risk_score": <0-100>, "confidence": <0.0-1.0>}}

    in:  {"method": "execute", "params": {"verb": "trade.execute", "payload": {...}}}
    out: {"ok": true, "result": {"status": "filled", ...}}

`payload` fields: `symbol` (str), `side` ("buy"|"sell"), `amount` (usd,
float). A missing/invalid `amount` is treated as a validation failure,
returned as `{"ok": false, "error": ...}` -- never a silent default.
"""
import json
import sys

# Above this notional size a trade is scored maximally risky (100) --
# deliberately a real, tunable formula, not the hardcoded `0` the social
# adapter uses today (see ta-submit::social_adapter.rs::publish).
MAX_RISK_NOTIONAL_USD = 2000.0

# A mock adapter still has a bounded confidence in its own risk model --
# never the same as a validator's self-reported certainty about a specific
# trade's outcome, so it is fixed rather than derived.
RISK_MODEL_CONFIDENCE = 0.9


def compute_risk_score(payload):
    amount = payload.get("amount")
    if not isinstance(amount, (int, float)) or amount < 0:
        raise ValueError(f"payload.amount must be a non-negative number, got {amount!r}")
    symbol = payload.get("symbol", "")

    notional_risk = min(100, round(100 * amount / MAX_RISK_NOTIONAL_USD))
    # A very short ticker reads as a thinly-traded / speculative symbol in
    # this mock model -- a small, explainable bump, not a hardcoded value.
    illiquidity_bump = 10 if len(symbol) <= 2 else 0
    return min(100, notional_risk + illiquidity_bump)


def simulate_fill(payload):
    symbol = payload.get("symbol", "UNKNOWN")
    side = payload.get("side", "buy")
    amount = payload.get("amount")
    if not isinstance(amount, (int, float)) or amount < 0:
        raise ValueError(f"payload.amount must be a non-negative number, got {amount!r}")

    # Deterministic pseudo-price so repeated calls with the same symbol are
    # reproducible in tests -- not a real market quote.
    pseudo_price = 10.0 + (sum(ord(c) for c in symbol) % 90)
    quantity = round(amount / pseudo_price, 4) if pseudo_price else 0.0

    return {
        "status": "filled",
        "symbol": symbol,
        "side": side,
        "amount": amount,
        "fill_price": pseudo_price,
        "quantity": quantity,
        "venue": "paper-trading-mock",
    }


def main():
    line = sys.stdin.readline()
    if not line.strip():
        print(json.dumps({"ok": False, "error": "no request line on stdin"}))
        return

    try:
        request = json.loads(line)
        method = request["method"]
        params = request.get("params", {})
        payload = params.get("payload", {})
    except (json.JSONDecodeError, KeyError) as e:
        print(json.dumps({"ok": False, "error": f"malformed request: {e}"}))
        return

    try:
        if method == "risk_score":
            result = {
                "risk_score": compute_risk_score(payload),
                "confidence": RISK_MODEL_CONFIDENCE,
            }
        elif method == "execute":
            result = simulate_fill(payload)
        else:
            print(json.dumps({"ok": False, "error": f"unknown method '{method}'"}))
            return
        print(json.dumps({"ok": True, "result": result}))
    except ValueError as e:
        print(json.dumps({"ok": False, "error": str(e)}))


if __name__ == "__main__":
    main()
