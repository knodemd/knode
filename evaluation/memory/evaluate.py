#!/usr/bin/env python3
"""Validate normalized end-to-end trials and report paired memory-workflow comparisons.

No API calls, token estimation or personal-data collection. Provider usage must be supplied
by an authorized runner; absent usage stays unknown. Input includes cached tokens; output
includes reasoning tokens. Normalize provider-specific fields before submitting results.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import random
import statistics

ARMS = {"files-full", "files-selective", "knode", "knode-bounded"}


def corpus_hash(root):
    digest = hashlib.sha256()
    for path in sorted(root.rglob("*.md")):
        digest.update(path.relative_to(root).as_posix().encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def number(value, name, integer=False):
    if isinstance(value, bool) or not isinstance(value, int if integer else (int, float)) or value < 0 or not math.isfinite(value):
        raise ValueError(f"{name} must be a finite nonnegative {'integer' if integer else 'number'}")
    return value


def validate(data, expected_hash, task_ids):
    if data.get("schema_version") != 1 or data.get("corpus_sha256") != expected_hash:
        raise ValueError("schema version or corpus hash does not match this benchmark")
    if not isinstance(data.get("model"), str) or not data["model"].strip():
        raise ValueError("one exact model/version is required per result file")
    runs = data.get("runs")
    if not isinstance(runs, list) or not runs:
        raise ValueError("runs must be a nonempty array")
    seen = set()
    for run in runs:
        if run.get("arm") not in ARMS or run.get("task") not in task_ids:
            raise ValueError("unknown arm or task")
        if not isinstance(run.get("trial"), str) or not run["trial"]:
            raise ValueError("each run needs a trial identifier")
        key = (run["arm"], run["task"], run["trial"])
        if key in seen:
            raise ValueError("duplicate arm/task/trial")
        seen.add(key)
        if run.get("scope") != "end-to-end" or run.get("includes_memory_writes") is not True:
            raise ValueError("include retrieval, work and memory-write costs in end-to-end usage")
        if not isinstance(run.get("success"), bool):
            raise ValueError("success must be independently scored true or false")
        if not isinstance(run.get("evidence"), str) or not run["evidence"].strip():
            raise ValueError("provide an answer/tool-trace reference or failure evidence")
        for metric in ("latency_ms", "tool_calls", "returned_chars"):
            value = run.get(metric)
            if value is not None:
                number(value, metric, integer=metric != "latency_ms")
        usage = run.get("usage")
        if usage is not None:
            if not isinstance(usage, dict):
                raise ValueError("usage must be an object or null")
            for field in ("input_tokens", "output_tokens"):
                number(usage.get(field), field, integer=True)
            for sub, total in (("cached_input_tokens", "input_tokens"), ("reasoning_output_tokens", "output_tokens")):
                value = usage.get(sub)
                if value is not None and number(value, sub, integer=True) > usage[total]:
                    raise ValueError(f"{sub} cannot exceed {total}")
    prices = data.get("prices")
    if prices is not None:
        if prices.get("currency") != "USD" or not prices.get("as_of") or not prices.get("source"):
            raise ValueError("prices require USD currency, date and source")
        for name in ("input_per_million", "cached_input_per_million", "output_per_million"):
            number(prices.get(name), name)
    return runs


def total_tokens(run):
    usage = run.get("usage")
    return None if usage is None else usage["input_tokens"] + usage["output_tokens"]


def cost(run, prices):
    usage = run.get("usage")
    if prices is None or usage is None or usage.get("cached_input_tokens") is None:
        return None
    cached = usage["cached_input_tokens"]
    return ((usage["input_tokens"] - cached) * prices["input_per_million"]
            + cached * prices["cached_input_per_million"]
            + usage["output_tokens"] * prices["output_per_million"]) / 1_000_000


def summary(values):
    present = [v for v in values if v is not None]
    return {"measured_runs": len(present), "mean": statistics.mean(present) if present else None,
            "median": statistics.median(present) if present else None}


def interval(values):
    if len(values) < 2:
        return None
    rng = random.Random(0)
    means = sorted(statistics.mean(rng.choices(values, k=len(values))) for _ in range(2000))
    return [means[49], means[1949]]


def report(data, expected_hash, task_ids):
    runs = validate(data, expected_hash, task_ids)
    arms = {}
    for arm in sorted({r["arm"] for r in runs}):
        group = [r for r in runs if r["arm"] == arm]
        arms[arm] = {"runs": len(group), "successes": sum(r["success"] for r in group),
                     "total_tokens": summary([total_tokens(r) for r in group]),
                     "estimated_usd": summary([cost(r, data.get("prices")) for r in group])}
        for metric in ("latency_ms", "tool_calls", "returned_chars"):
            arms[arm][metric] = summary([r.get(metric) for r in group])
    comparisons = []
    for base in ("files-full", "files-selective"):
        index = {(r["task"], r["trial"]): r for r in runs if r["arm"] == base}
        for candidate in ("knode", "knode-bounded"):
            pairs = [(index[(r["task"], r["trial"])], r) for r in runs
                     if r["arm"] == candidate and (r["task"], r["trial"]) in index]
            if not pairs:
                continue
            differences = [total_tokens(b) - total_tokens(c) for b, c in pairs
                           if total_tokens(b) is not None and total_tokens(c) is not None]
            comparisons.append({"baseline": base, "candidate": candidate, "paired_runs": len(pairs),
                                "baseline_successes": sum(b["success"] for b, _ in pairs),
                                "candidate_successes": sum(c["success"] for _, c in pairs),
                                "tokens_saved": summary(differences),
                                "tokens_saved_bootstrap_95pct": interval(differences)})
    return {"model": data["model"], "corpus_sha256": expected_hash, "arms": arms,
            "paired_comparisons": comparisons,
            "limitations": ["No unpaired cross-task average is evidence of savings; inspect paired success and failures.",
                            "Unknown usage is not zero and returned characters are not token counts.",
                            "Bootstrap intervals need enough independent repetitions; small samples are inconclusive.",
                            "Costs require user-supplied dated prices and cache counts; subscriptions may not bill per token.",
                            "Report setup/indexing separately and include all retrieval and memory-writing usage in each run."]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path)
    args = parser.parse_args()
    root = Path(__file__).parent
    tasks = json.loads((root / "tasks.json").read_text())
    try:
        result = report(json.loads(args.results.read_text()), corpus_hash(root / "corpus"), {t["id"] for t in tasks})
    except (ValueError, TypeError, KeyError) as error:
        parser.error(str(error))
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
