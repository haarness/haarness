#!/usr/bin/env python3
"""Check Haarness's model prices against OpenRouter's public catalogue, as an aid to the routine manual review.

    python3 scripts/check-model-prices.py            # report; exit 1 when something needs a look
    python3 scripts/check-model-prices.py --all      # also list models released more than a year ago
    python3 scripts/check-model-prices.py --sync     # first copy haarness-cloud's table over the app's copy

Prices come from two places, in Haarness Cloud and in the app alike. The reviewed table, `model-prices.json`, holds
provider list prices checked by hand against each provider's price page; Cloud keeps it in `config/` and the app
embeds an identical copy in `crates/haarness-providers/src/`. Every model the table does not list is priced from
OpenRouter's catalogue: Cloud imports it hourly, the app keeps a snapshot in `catalog.rs`. Nothing reviews either
automatically, so this reports:

- table entries whose OpenRouter price differs: the provider changed its price, or OpenRouter is wrong (an entry's
  `note` explains a known difference, which is shown but not counted);
- table entries not checked for `--stale` days;
- recent OpenAI, Anthropic and Gemini models the table does not list, which Cloud bills at OpenRouter's price,
  and where the app's fallback price disagrees with it;
- whether the app's copy of the table matches haarness-cloud's.

To fix an entry, or to add a model, open the provider's page, write its numbers and today's `checked` date into
haarness-cloud's `config/model-prices.json`, run this with `--sync`, and deploy Cloud. Needs the network.
"""
import argparse
import datetime
import json
import re
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CATALOG = ROOT / "crates/haarness-providers/src/catalog.rs"
TABLE = ROOT / "crates/haarness-providers/src/model-prices.json"
CLOUD_TABLE = ROOT.parent / "haarness-cloud/config/model-prices.json"
SOURCE = "https://openrouter.ai/api/v1/models"
PROVIDERS = {"openai": "openai", "anthropic": "anthropic", "google": "gemini"}
# Listed under a provider's name but never called through that provider.
IGNORED = {
    "openai/gpt-audio": "audio-only",
    "openai/gpt-oss": "open weights, priced by whichever host serves them",
}
# OpenRouter names for GPT `reasoning.mode: "pro"`; OpenAI has no such model ids.
IGNORED_PATTERN = re.compile(r"^openai/gpt-(5\.6|6)[^/]*-pro$")
SNAPSHOT = re.compile(r"-(\d{8}|\d{4}-\d{2}-\d{2})")


def price_key(model_id):
    """As in catalog.rs: the vendor is dropped and dots read as dashes."""
    return model_id.rsplit("/", 1)[-1].lower().replace(".", "-")


def reviewed(table, model_id):
    """The app's match: an entry's id or alias, or a dated snapshot of one."""
    key = price_key(model_id)
    for entry in table:
        for name in (entry["id"], *entry.get("aliases", [])):
            name = price_key(name)
            if key == name or (key.startswith(name) and SNAPSHOT.fullmatch(key[len(name) :])):
                return entry
    return None


def catalog_entries():
    source = CATALOG.read_text()
    body = source[source.index("const CATALOG") : source.index("const LONG_CONTEXT")]
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


def close(ours, listed):
    """OpenRouter rounds some cache prices to the cent (0.125 → 0.13)."""
    return abs(ours - listed) <= 0.005 + 1e-9


def listed_prices(model, provider):
    """OpenRouter's (input, output, cache read, cache write or None, first long-context tier or None)."""
    pricing = model["pricing"]
    tier = min(pricing.get("overrides") or [], key=lambda t: int(t.get("min_prompt_tokens", 0)), default=None)
    return (
        per_million(pricing.get("prompt")),
        per_million(pricing.get("completion")),
        per_million(pricing.get("input_cache_read")),
        # Gemini bills cache storage per hour; OpenRouter's figure is not a per-token write price.
        per_million(pricing.get("input_cache_write")) if provider != "gemini" else None,
        tier
        and (
            int(tier["min_prompt_tokens"]),
            per_million(tier.get("prompt")),
            per_million(tier.get("completion")),
        ),
    )


def differences(ours, listed):
    """Where `ours` = (input, output, cache read, cache write, (above, input, output) or None) disagrees with OpenRouter."""
    found = []
    for name, mine, theirs in zip(("input", "output", "cache read", "cache write"), ours[:4], listed[:4]):
        if theirs and not close(mine, theirs):
            found.append(f"{name} {mine:g} vs {theirs:g}")
    tier, listed_tier = ours[4], listed[4]
    if listed_tier and not tier:
        found.append(f"no long-context tier, OpenRouter {listed_tier[1]:g}/{listed_tier[2]:g} from {listed_tier[0]}")
    elif tier and not listed_tier:
        found.append("long-context tier not on OpenRouter")
    elif tier and (abs(tier[0] - listed_tier[0]) > 1 or not all(map(close, tier[1:], listed_tier[1:]))):
        found.append(f"tier {tier[1]:g}/{tier[2]:g} over {tier[0]} vs {listed_tier[1]:g}/{listed_tier[2]:g} from {listed_tier[0]}")
    return found


def table_prices(entry):
    tier = entry.get("long_context")
    return (
        float(entry["input"]),
        float(entry["output"]),
        float(entry["cache_read"]),
        float(entry["cache_write"]),
        tier and (tier["above_input_tokens"], float(tier["input"]), float(tier["output"])),
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--all", action="store_true", help="also list models released more than a year ago")
    parser.add_argument("--days", type=int, default=365, help="what counts as a recent release (default 365)")
    parser.add_argument("--stale", type=int, default=30, help="days before an entry is due a check (default 30)")
    parser.add_argument("--sync", action="store_true", help="copy haarness-cloud's config/model-prices.json first")
    args = parser.parse_args()

    attention = 0
    if CLOUD_TABLE.exists():
        if args.sync and CLOUD_TABLE.read_bytes() != TABLE.read_bytes():
            TABLE.write_bytes(CLOUD_TABLE.read_bytes())
            print(f"Copied {CLOUD_TABLE} to {TABLE.relative_to(ROOT)}.")
        elif CLOUD_TABLE.read_bytes() != TABLE.read_bytes():
            print(f"The app's model-prices.json differs from {CLOUD_TABLE}; run with --sync.\n")
            attention += 1
    elif args.sync:
        print(f"{CLOUD_TABLE} not found; nothing to sync.", file=sys.stderr)
        return 1

    table = json.loads(TABLE.read_text())["models"]
    with urllib.request.urlopen(SOURCE, timeout=30) as response:
        models = {model["id"]: model for model in json.load(response)["data"]}
    today = datetime.date.today()

    print("Reviewed table (model-prices.json):")
    for entry in table:
        provider = PROVIDERS.get(entry["id"].split("/")[0])
        age = (today - datetime.date.fromisoformat(entry["checked"])).days
        stale = [f"checked {age} days ago"] if age > args.stale else []
        found = [] if entry["id"] in models else ["not on OpenRouter"]
        for name in (entry["id"], *entry.get("aliases", [])):
            if name in models:
                prefix = "" if name == entry["id"] else f"{name.split('/')[1]}: "
                found += [prefix + problem for problem in differences(table_prices(entry), listed_prices(models[name], provider))]
        # A difference the entry's note explains is shown, not counted; so is a model OpenRouter lacks.
        known = entry.get("note") if found else None
        attention += bool(stale) or (not known and any(problem != "not on OpenRouter" for problem in found))
        if stale or found:
            print(f"  {entry['id']}: " + "; ".join(stale + found) + (f"\n      note: {known}" if known else ""))
    print(f"  {len(table)} entries; see each entry's `source` for its price page.\n")

    entries = catalog_entries()
    tiers = long_context_entries()
    cutoff = datetime.datetime.now().timestamp() - args.days * 86_400
    listed_ids = {name for entry in table for name in (entry["id"], *entry.get("aliases", []))}
    rows = []
    for model_id, model in sorted(models.items()):
        org, _, name = model_id.partition("/")
        provider = PROVIDERS.get(org)
        if provider is None or ":" in name or (provider == "gemini" and not name.startswith("gemini")):
            continue
        if model_id in listed_ids or any(model_id.startswith(p) for p in IGNORED) or IGNORED_PATTERN.match(model_id):
            continue
        recent = model.get("created", 0) >= cutoff
        if not (recent or args.all):
            continue
        listed = listed_prices(model, provider)
        if (entry := reviewed(table, model_id)) is not None:
            # A dated snapshot: the app uses the entry; Cloud bills OpenRouter's price.
            app = f"app uses {entry['id']}"
            app_problems = differences(table_prices(entry), listed)
        elif (fallback := lookup(entries, provider, name)) is None or fallback[1] is None:
            app, app_problems = "app shows no price", ["no app price"]
        else:
            price = fallback[1]
            tier = tiers.get((provider, fallback[0]))
            app = f"app via {fallback[0]}"
            app_problems = differences((*price, tier and tier[:3]), listed)
        released = datetime.date.fromtimestamp(model.get("created", 0))
        rows.append((recent, model_id, listed, app, app_problems, released))

    print("Priced by OpenRouter (not in the table):")
    for recent, model_id, listed, app, app_problems, released in rows:
        attention += recent and bool(app_problems)
        tier = f", {listed[4][1]:g}/{listed[4][2]:g} from {listed[4][0]}" if listed[4] else ""
        detail = "; ".join(app_problems) or "matches"
        print(f"  {model_id}: {listed[0]:g}/{listed[1]:g}{tier} (released {released}); {app}: {detail}")
    print(f"  {len(rows)} models.\n")
    print(f"{attention} item(s) need a look." if attention else "Nothing needs a look.")
    return 1 if attention else 0


if __name__ == "__main__":
    sys.exit(main())
