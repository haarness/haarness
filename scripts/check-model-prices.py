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

- price changes the table schedules (an entry's `changes`), flagging one due within `--due` days so its new price
  is confirmed on the provider's page before Cloud starts billing it, and one that adds or ends a long-context tier;
- table entries whose OpenRouter price differs from today's rates: the provider changed its price, or OpenRouter is
  wrong (an entry's `note` explains a known difference, and OpenRouter may already list a scheduled change's price;
  both are shown but not counted);
- table entries not checked for `--stale` days;
- recent OpenAI, Anthropic and Gemini models the table does not list, which Cloud bills at OpenRouter's price,
  and where the app's fallback price disagrees with it;
- whether the app's copy of the table matches haarness-cloud's.

To fix an entry, or to add a model, open the provider's page, write its numbers and today's `checked` date into
haarness-cloud's `config/model-prices.json`, run this with `--sync`, and deploy Cloud. When a provider announces a
price for a later date, add it to the entry's `changes` instead: `{"from": "YYYY-MM-DD", ...}` with every rate,
`long_context` included, billed from 00:00 UTC that day. Needs the network for OpenRouter's catalogue; nothing is
fetched from the providers' own pages.
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


def rates_on(entry, day):
    """The rates billed on `day` (UTC): the latest change whose `from` date has come, else the entry's own."""
    started = [change for change in entry.get("changes", []) if datetime.date.fromisoformat(change["from"]) <= day]
    return max(started, key=lambda change: change["from"], default=entry)


def table_prices(rates):
    tier = rates.get("long_context")
    return (
        float(rates["input"]),
        float(rates["output"]),
        float(rates["cache_read"]),
        float(rates["cache_write"]),
        tier and (tier["above_input_tokens"], float(tier["input"]), float(tier["output"])),
    )


def rate_changes(old, new):
    """What a scheduled change does to the rates before it, e.g. `2/10 → 4/20, cache read 0.1 → 0.2`."""
    (oi, oo, ocr, ocw, otier), (ni, no, ncr, ncw, ntier) = table_prices(old), table_prices(new)
    found = [f"{oi:g}/{oo:g} → {ni:g}/{no:g}"] if (oi, oo) != (ni, no) else []
    found += [f"{name} {a:g} → {b:g}" for name, a, b in (("cache read", ocr, ncr), ("cache write", ocw, ncw)) if a != b]
    if otier and not ntier:
        found.append("ends the long-context tier")
    elif ntier and not otier:
        found.append(f"adds a long-context tier, {ntier[1]:g}/{ntier[2]:g} over {ntier[0]}")
    elif old.get("long_context") != new.get("long_context"):
        found.append(f"long-context {otier[1]:g}/{otier[2]:g} over {otier[0]} → {ntier[1]:g}/{ntier[2]:g} over {ntier[0]}")
    return ", ".join(found) or "no rate changes"


def schedule_problems(entry):
    """Changes Cloud would refuse (or misread): a missing rate, a bad date, dates out of order."""
    problems, previous = [], None
    for change in entry.get("changes", []):
        missing = [name for name in ("from", "input", "output", "cache_read", "cache_write") if name not in change]
        if missing:
            problems.append(f"a change lacks {', '.join(missing)}")
            continue
        try:
            day = datetime.date.fromisoformat(change["from"])
        except ValueError:
            day = None
        if day is None or not re.fullmatch(r"\d{4}-\d{2}-\d{2}", change["from"]):
            problems.append(f"`from` {change['from']!r} is not YYYY-MM-DD")
            continue
        if previous and day <= previous:
            problems.append(f"changes out of date order at {day}")
        previous = day
    return problems


def report_changes(table, today, due):
    """Print the table's scheduled changes in date order; returns how many need a look."""
    print("Scheduled price changes (an entry's `changes`):")
    attention, rows = 0, []
    for entry in table:
        if problems := schedule_problems(entry):
            print(f"  {entry['id']}: " + "; ".join(problems))
            attention += 1
            continue
        before = entry
        for change in entry.get("changes", []):
            rows.append((change["from"], entry, before, change))
            before = change
    for start, entry, before, change in sorted(rows, key=lambda row: row[0]):
        days = (datetime.date.fromisoformat(start) - today).days
        what = rate_changes(before, change)
        if days <= 0:
            when = "in force; fold it into the entry's own rates at its next check"
        elif days <= due:
            when = f"in {days} days: confirm it on {entry['source']}"
            attention += 1
        else:
            when = f"in {days} days"
        # A change that adds or ends a tier is unusual: counted unless the entry's note speaks to it.
        tiers = "long-context tier" in what and days > 0 and not entry.get("note")
        attention += tiers
        print(f"  {start} {entry['id']}: {what}; {when}" + (" (check the tier)" if tiers else ""))
    print(f"  {len(rows)} scheduled; each is due a look {due} days before it takes effect at 00:00 UTC.\n")
    return attention


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--all", action="store_true", help="also list models released more than a year ago")
    parser.add_argument("--days", type=int, default=365, help="what counts as a recent release (default 365)")
    parser.add_argument("--stale", type=int, default=30, help="days before an entry is due a check (default 30)")
    parser.add_argument("--due", type=int, default=14, help="days before a scheduled change is due a look (default 14)")
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
    # Cloud prices each import by its UTC date.
    today = datetime.datetime.now(datetime.timezone.utc).date()
    attention += report_changes(table, today, args.due)
    table = [entry for entry in table if not schedule_problems(entry)]
    with urllib.request.urlopen(SOURCE, timeout=30) as response:
        models = {model["id"]: model for model in json.load(response)["data"]}

    print("Reviewed table (model-prices.json):")
    for entry in table:
        provider = PROVIDERS.get(entry["id"].split("/")[0])
        age = (today - datetime.date.fromisoformat(entry["checked"])).days
        stale = [f"checked {age} days ago"] if age > args.stale else []
        found = [] if entry["id"] in models else ["not on OpenRouter"]
        upcoming = [change for change in entry.get("changes", []) if datetime.date.fromisoformat(change["from"]) > today]
        ahead = None
        for name in (entry["id"], *entry.get("aliases", [])):
            if name in models:
                prefix = "" if name == entry["id"] else f"{name.split('/')[1]}: "
                listed = listed_prices(models[name], provider)
                problems = differences(table_prices(rates_on(entry, today)), listed)
                if problems and (
                    change := next((c for c in upcoming if not differences(table_prices(c), listed)), None)
                ):
                    ahead = f"OpenRouter already lists the price from {change['from']}"
                found += [prefix + problem for problem in problems]
        # A difference the entry's note or schedule explains is shown, not counted; so is a model OpenRouter lacks.
        known = (ahead or entry.get("note")) if found else None
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
            app_problems = differences(table_prices(rates_on(entry, today)), listed)
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
