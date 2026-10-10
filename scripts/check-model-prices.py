#!/usr/bin/env python3
"""Compare the built-in model price table with the providers' current list prices.

    python3 scripts/check-model-prices.py            # report, exit 1 on a recent model with a wrong or missing price
    python3 scripts/check-model-prices.py --all      # also list old and unpriced models

The runtime shows a cost on every turn from `crates/haarness-providers/src/catalog.rs`. New releases (and `-pro`,
`-mini` or `-image` variants that cost differently from their family) silently borrow the nearest prefix's price, or
none. Input, output and cache read prices are compared, and cache writes for OpenAI and Anthropic. This resolves each OpenAI, Anthropic and Gemini model in OpenRouter's public catalogue (which passes list prices
through) the way `catalog::lookup` does, longest prefix first, and reports every disagreement. Needs the network.
"""
import argparse
import datetime
import json
import re
import sys
import urllib.request
from pathlib import Path

CATALOG = Path(__file__).resolve().parent.parent / "crates/haarness-providers/src/catalog.rs"
SOURCE = "https://openrouter.ai/api/v1/models"
# Listed under a provider's name but never called through that provider by the runtime.
IGNORED = {
    "openai/gpt-audio": "audio-only",
    "openai/gpt-oss": "open weights, priced by whichever host serves them",
}
# OpenRouter names for GPT `reasoning.mode: "pro"`; OpenAI has no such model ids.
IGNORED_PATTERN = re.compile(r"^openai/gpt-(5\.6|6)[^/]*-pro$")
# Where OpenRouter disagrees with the provider's own price page, the page wins.
PROVIDER_PRICES = {
    # developers.openai.com/api/docs/pricing, 2026-10-10: promotional through at least 2026-11-21.
    "openai/gpt-5.6-sol": (4.0, 20.0),
}


def catalog_entries():
    source = CATALOG.read_text()
    body = source[source.index("const CATALOG") : source.index("pub fn lookup")]
    entries = {}
    for provider, block in re.findall(r'\(\s*"(\w+)",\s*&\[(.*?)\],\s*\)', body, re.S):
        for match in re.finditer(
            r'entry\(\s*"([^"]+)",\s*[\d_]+,\s*[\d_]+,\s*(None|Some\(\(([^)]*)\)\))', block, re.S
        ):
            price = None if match.group(2) == "None" else tuple(float(x) for x in match.group(3).split(","))
            entries.setdefault(provider, []).append((match.group(1), price))
    return entries


def long_context_entries():
    source = CATALOG.read_text()
    body = source[source.index("const LONG_CONTEXT") : source.index("fn long_context")]
    return {
        (provider, prefix): (int(above.replace("_", "")), float(i), float(o), float(r), float(w))
        for provider, prefix, above, i, o, r, w in re.findall(
            r'\("(\w+)", "([^"]+)", ([\d_]+), ([\d.]+), ([\d.]+), ([\d.]+), ([\d.]+)\)', body
        )
    }


def lookup(entries, provider, model):
    if provider == "anthropic":
        model = model.replace(".", "-")
    matches = [entry for entry in entries.get(provider, []) if model.startswith(entry[0])]
    return max(matches, key=lambda entry: len(entry[0]), default=None)


def per_million(value):
    return round(float(value) * 1_000_000, 4) if value not in (None, "") else 0.0


def tier_problem(overrides, ours, prefix):
    """The long-context tier against OpenRouter's first `pricing.overrides` entry (thresholds may differ by one)."""
    listed = min(overrides or [], key=lambda tier: int(tier.get("min_prompt_tokens", 0)), default=None)
    if listed is None:
        return f"tier {ours[:3]} via {prefix} not listed" if ours else None
    rates = (per_million(listed.get("prompt")), per_million(listed.get("completion")))
    if ours is None:
        return f"no long-context tier via {prefix}, list {rates} from {listed.get('min_prompt_tokens')} tokens"
    if abs(ours[0] - int(listed["min_prompt_tokens"])) > 1 or not all(map(close, ours[1:3], rates)):
        return f"tier {ours[:3]} via {prefix}, list {rates} from {listed['min_prompt_tokens']} tokens"
    return None


def close(ours, listed):
    """OpenRouter rounds some cache prices to the cent (0.125 → 0.13)."""
    return abs(ours - listed) <= 0.005 + 1e-9


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--all", action="store_true", help="also report models released more than a year ago")
    parser.add_argument("--days", type=int, default=365, help="what counts as a recent release (default 365)")
    args = parser.parse_args()

    with urllib.request.urlopen(SOURCE, timeout=30) as response:
        models = json.load(response)["data"]
    entries = catalog_entries()
    tiers = long_context_entries()
    cutoff = datetime.datetime.now().timestamp() - args.days * 86_400
    problems, checked = [], 0
    for model in models:
        org, _, name = model["id"].partition("/")
        if ":" in name:  # batch and other priced variants the runtime never calls
            continue
        provider = {"openai": "openai", "anthropic": "anthropic", "google": "gemini"}.get(org)
        if provider is None or (provider == "gemini" and not name.startswith("gemini")):
            continue
        if any(model["id"].startswith(prefix) for prefix in IGNORED) or IGNORED_PATTERN.match(model["id"]):
            continue
        pricing = model["pricing"]
        listed = PROVIDER_PRICES.get(model["id"]) or (
            per_million(pricing.get("prompt")),
            per_million(pricing.get("completion")),
        )
        cached = per_million(pricing.get("input_cache_read")) if model["id"] not in PROVIDER_PRICES else 0
        # Gemini bills cache storage per hour, which the table keeps at 0 (see catalog.rs).
        written = (
            per_million(pricing.get("input_cache_write"))
            if provider != "gemini" and model["id"] not in PROVIDER_PRICES
            else None
        )
        entry = lookup(entries, provider, name)
        recent = model.get("created", 0) >= cutoff
        checked += 1
        if entry is None:
            problem = "no entry"
        elif entry[1] is None:
            problem = f"no price (entry {entry[0]})"
        elif entry[1][:2] != listed:
            problem = f"priced {entry[1][:2]} via {entry[0]}"
        elif cached and not close(entry[1][2], cached):
            problem = f"cache read {entry[1][2]} via {entry[0]}, list {cached}"
        elif written and not close(entry[1][3], written):
            problem = f"cache write {entry[1][3]} via {entry[0]}, list {written}"
        elif model["id"] not in PROVIDER_PRICES and (
            problem := tier_problem(pricing.get("overrides"), tiers.get((provider, entry[0])), entry[0])
        ):
            pass
        else:
            continue
        if recent or args.all:
            released = datetime.date.fromtimestamp(model.get("created", 0))
            problems.append((recent, f"{provider}/{name}: list {listed}, {problem} (released {released})"))

    for _, line in sorted(problems, key=lambda problem: not problem[0]):
        print(line)
    recent = sum(1 for is_recent, _ in problems if is_recent)
    print(f"{checked} models checked; {recent} recent model(s) with a wrong or missing price.")
    return 1 if recent else 0


if __name__ == "__main__":
    sys.exit(main())
