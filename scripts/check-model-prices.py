#!/usr/bin/env python3
"""Compare the built-in model price table with the providers' current list prices.

    python3 scripts/check-model-prices.py            # report, exit 1 on a recent model with a wrong or missing price
    python3 scripts/check-model-prices.py --all      # also list old and unpriced models

The runtime shows a cost on every turn from `crates/haarness-providers/src/catalog.rs`. New releases (and `-pro`,
`-mini` or `-image` variants that cost differently from their family) silently borrow the nearest prefix's price, or
none. This resolves each OpenAI, Anthropic and Gemini model in OpenRouter's public catalogue (which passes list prices
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


def lookup(entries, provider, model):
    if provider == "anthropic":
        model = model.replace(".", "-")
    matches = [entry for entry in entries.get(provider, []) if model.startswith(entry[0])]
    return max(matches, key=lambda entry: len(entry[0]), default=None)


def per_million(value):
    return round(float(value) * 1_000_000, 4) if value not in (None, "") else 0.0


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--all", action="store_true", help="also report models released more than a year ago")
    parser.add_argument("--days", type=int, default=365, help="what counts as a recent release (default 365)")
    args = parser.parse_args()

    with urllib.request.urlopen(SOURCE, timeout=30) as response:
        models = json.load(response)["data"]
    entries = catalog_entries()
    cutoff = datetime.datetime.now().timestamp() - args.days * 86_400
    problems, checked = [], 0
    for model in models:
        org, _, name = model["id"].partition("/")
        if ":" in name:  # batch and other priced variants the runtime never calls
            continue
        provider = {"openai": "openai", "anthropic": "anthropic", "google": "gemini"}.get(org)
        if provider is None or (provider == "gemini" and not name.startswith("gemini")):
            continue
        if any(model["id"].startswith(prefix) for prefix in IGNORED):
            continue
        pricing = model["pricing"]
        listed = (per_million(pricing.get("prompt")), per_million(pricing.get("completion")))
        entry = lookup(entries, provider, name)
        recent = model.get("created", 0) >= cutoff
        checked += 1
        if entry is None:
            problem = "no entry"
        elif entry[1] is None:
            problem = f"no price (entry {entry[0]})"
        elif entry[1][:2] != listed:
            problem = f"priced {entry[1][:2]} via {entry[0]}"
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
