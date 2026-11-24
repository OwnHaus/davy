"""Turns the runs of bench/footprint.sh into bench/footprint.json and prints
the tables for docs/storage-benchmark.md.

python3 -I bench/footprint-report.py RUNS OUT COMMIT SHARED

OUT is written only when every run is complete. SHARED is 1 if other work
ran on the machine at the same time, 0 if not, empty if unknown.
"""

import json
import os
import platform
import statistics
import subprocess
import sys
import time

SCHEMA = "davy.footprint"
VERSION = 1
# A corpus this small is mostly the fixed cost of a day index and says
# little about the bytes of a line.
MIN_LINES = 20_000
# A run this long gives the disk sampler time to see the peaks.
BIG_LINES = 500_000
# The tables an error event touches; footprint.sh counts each index with
# its table, and the FTS5 shadow tables of issues_text with the issue.
EVENT_TABLES = ("events",)
ISSUE_TABLES = ("issues", "issues_text", "issues_text_data", "issues_text_idx",
                "issues_text_docsize", "issues_text_config", "issues_text_content")
HOURLY_TABLES = ("issue_hourly",)
TAG_TABLES = ("issue_tags",)
# The payload sampling of src/store/latest.rs: an issue keeps the payload of
# its first 50 events, of every 100th after, and of its newest. Each error
# run checks that the stored payloads follow it.
FIRST_PAYLOADS = 50
PAYLOAD_EVERY = 100


def kept_payloads(events):
    """The payloads an issue of `events` events keeps."""
    sampled = lambda n: n < FIRST_PAYLOADS or (n + 1) % PAYLOAD_EVERY == 0
    return sum(map(sampled, range(events))) + (0 if sampled(events - 1) else 1)


def read_runs(root):
    runs = []
    for name in sorted(os.listdir(root)):
        path = os.path.join(root, name, "result.env")
        if not os.path.exists(path):
            continue
        r = {}
        for line in open(path):
            k, _, v = line.rstrip("\n").partition("=")
            try:
                r[k] = int(v)
            except ValueError:
                try:
                    r[k] = float(v)
                except ValueError:
                    r[k] = v
        runs.append(r)
    return runs


def spread(values):
    values = sorted(values)
    if not values:
        return None
    r = lambda v: round(v, 4) if abs(v) < 100 else round(v)
    return {"min": r(values[0]), "median": r(statistics.median(values)), "max": r(values[-1]), "n": len(values)}


def logs_row(r):
    lines, raw, disk = r["lines"], r["raw_bytes"], r["disk_bytes"]
    total = r["tantivy_bytes"] or 1
    share = {k[len("field_"):-len("_bytes")]: round(v / total, 4)
             for k, v in r.items() if k.startswith("field_") and k.endswith("_bytes")}
    share["store"] = round(r["store_bytes"] / total, 4)
    row = {
        "corpus": r["corpus"], "shape": r["shape"], "days": r["days"],
        "source": r["source"], "sha256": r["sha256"] or None, "member": r["member"] or None,
        "corpus_sha256": r["corpus_sha256"], "load_average_at_start": r["load_at_start"],
        "lines": lines, "raw_bytes": raw, "raw_bytes_per_line": round(raw / lines, 1),
        "stored_docs": r["docs"], "complete": r["docs"] == lines,
        "segments": r["segments"], "day_indexes": r["day_indexes"],
        "disk_bytes": disk, "disk_per_raw": round(disk / raw, 4),
        "disk_bytes_per_line": round(disk / lines, 1),
        "ingest_peak_extra_disk_bytes": max(0, r["ingest_peak_disk_bytes"] - disk),
        "share_of_index": share,
        "ingest_cpu_us_per_row": round(r["ingest_cpu_secs"] * 1e6 / lines, 2),
        "ingest_accept_rows_per_s": r["accept_rows_per_s"],
        "ingest_peak_rss_bytes": r["ingest_peak_rss_bytes"],
        "throttled": r["throttled"],
    }
    if "one_segment_bytes" in r:
        row["one_segment_bytes"] = r["one_segment_bytes"]
        row["one_segment_per_raw"] = round(r["one_segment_bytes"] / raw, 4)
        row["merge_peak_extra_disk_bytes"] = max(0, r["merge_peak_disk_bytes"] - disk)
        row["merge_secs"] = r["merge_secs"]
    if "search_rss_start_bytes" in r:
        row["search"] = {k: v for k, v in r.items() if k.startswith("search_")}
        row["retention"] = {
            "kept_days": 30,
            "days_before": r["retention_before_days"], "days_after": r["retention_after_days"],
            "freed_bytes": r["retention_before_bytes"] - r["retention_after_bytes"],
            "secs": r["retention_secs"],
        }
    return row


def size(r, names):
    return sum(r.get(f"table_{n}_bytes", 0) for n in names)


def errors_row(r):
    n, issues = r["stored_events"], r["stored_issues"]
    payload = r["payload_bytes"]
    row = (size(r, EVENT_TABLES) - payload) / n
    return {
        "issues": r["issues"], "events_per_issue": r["per_issue"], "days": r["days"],
        "envelope": r["envelope"], "events": r["events"], "stored_events": n,
        "complete": n == r["events"],
        "sqlite_bytes": r["sqlite_bytes"], "sqlite_bytes_per_event": round(r["sqlite_bytes"] / n, 1),
        "row_bytes_per_event": round(row, 1),
        "events_unused_share": round(r["events_unused_bytes"] / r["events_btree_bytes"], 4),
        "compacted_sqlite_bytes": r["compacted_sqlite_bytes"],
        "compacted_row_bytes_per_event": round((r["compacted_table_events_bytes"] - payload) / n, 1),
        "payloads": r["payloads"], "payloads_per_issue": round(r["payloads"] / issues, 1),
        "raw_event_bytes": r["raw_event_bytes"],
        "bytes_per_payload": round(payload / max(1, r["payloads"]), 1),
        "payloads_follow_sampling": r["payloads"] == issues * kept_payloads(r["per_issue"]),
        "payload_per_raw": round(payload / max(1, r["payloads"]) / r["raw_event_bytes"], 4),
        "hourly_rows": r["hourly_rows"],
        "bytes_per_hourly_row": round(size(r, HOURLY_TABLES) / max(1, r["hourly_rows"]), 1),
        "tag_rows": r["tag_rows"],
        "tag_bytes_per_issue": round(size(r, TAG_TABLES) / issues, 1),
        "tag_bytes_per_row": round(size(r, TAG_TABLES) / max(1, r["tag_rows"]), 1),
        "issue_bytes_per_issue": round(size(r, ISSUE_TABLES) / issues, 1),
        "ingest_cpu_us_per_event": round(r["ingest_cpu_secs"] * 1e6 / n, 1),
        "ingest_accept_events_per_s": r["accept_events_per_s"],
        "ingest_peak_rss_bytes": r["ingest_peak_rss_bytes"],
        "throttled": r["throttled"],
        "load_average_at_start": r["load_at_start"],
    }


def machine(shared):
    sysctl = lambda k: subprocess.run(["sysctl", "-n", k], capture_output=True, text=True).stdout.strip()
    return {
        "model": sysctl("hw.model"), "cpu": sysctl("machdep.cpu.brand_string"),
        "cores": int(sysctl("hw.ncpu")), "ram_bytes": int(sysctl("hw.memsize")),
        "os": f"macOS {platform.mac_ver()[0]}" if platform.mac_ver()[0] else f"{platform.system()} {platform.release()}",
        "load_average_at_report": sysctl("vm.loadavg"),
        "shared": {"1": True, "0": False}.get(shared),
    }


def coefficients(logs, errors):
    one_day = [r for r in logs if r["days"] == 1 and r["complete"]]
    real = [r for r in one_day if r["lines"] >= MIN_LINES and r["shape"] != "json"]
    synthetic = [r for r in one_day if r["shape"] == "json"]
    pairs = {r["corpus"]: r for r in one_day}
    # The same lines spread over more day indexes: what each extra day costs.
    per_day = [(r["disk_bytes"] - pairs[r["corpus"]]["disk_bytes"])
               / (r["day_indexes"] - pairs[r["corpus"]]["day_indexes"])
               for r in logs if r["days"] > 1 and r["corpus"] in pairs]
    # Only a run long enough for the 0.2 s disk sampler, and a merge that
    # had more than one segment to merge, says anything about headroom.
    big = [r for r in one_day if r["lines"] >= BIG_LINES]
    merged = [r for r in one_day if r["segments"] > 1 and "merge_peak_extra_disk_bytes" in r]
    by = lambda rows, key: spread([r[key] for r in rows])
    done = [e for e in errors if e["complete"]]
    # Every event of a cold issue keeps its payload; nearly no event of a hot
    # one does. SQLite pages are 4 KB, so per-issue bytes need many issues.
    cold = min(done, key=lambda e: e["events_per_issue"], default=None)
    hot = max(done, key=lambda e: e["events_per_issue"], default=None)
    many = [e for e in done if e["issues"] >= 1000]
    return {
        "logs": {
            "disk_per_raw_real": by(real, "disk_per_raw"),
            "disk_bytes_per_line_real": by(real, "disk_bytes_per_line"),
            "disk_per_raw_synthetic": by(synthetic, "disk_per_raw"),
            "values_share_of_index_synthetic": spread([r["share_of_index"].get("values", 0) for r in synthetic]),
            "one_segment_per_raw_real": by(real, "one_segment_per_raw"),
            "extra_bytes_per_day_index": spread(per_day),
            "ingest_peak_extra_disk_per_disk": spread([r["ingest_peak_extra_disk_bytes"] / r["disk_bytes"] for r in big]),
            "merge_peak_extra_disk_per_disk": spread([r["merge_peak_extra_disk_bytes"] / r["disk_bytes"] for r in merged]),
            "ingest_cpu_us_per_row": by(real + synthetic, "ingest_cpu_us_per_row"),
            "ingest_peak_rss_bytes": by(big, "ingest_peak_rss_bytes"),
            "peak_rss_bytes_2k_lines": by([r for r in one_day if r["lines"] < MIN_LINES], "ingest_peak_rss_bytes"),
        },
        "errors": {
            "event_with_payload": cold and {
                "measured_at_events_per_issue": cold["events_per_issue"],
                "row_bytes": cold["row_bytes_per_event"],
                "payload_per_raw_event_json": cold["payload_per_raw"],
            },
            "event_without_payload": hot and {
                "measured_at_events_per_issue": hot["events_per_issue"],
                "row_bytes_as_written": hot["row_bytes_per_event"],
                "row_bytes_compacted": hot["compacted_row_bytes_per_event"],
                "events_table_unused_share": hot["events_unused_share"],
            },
            "bytes_per_hourly_row": spread([e["bytes_per_hourly_row"] for e in done]),
            "bytes_per_tag_row": spread([e["tag_bytes_per_row"] for e in many]),
            "issue_bytes_per_issue": spread([e["issue_bytes_per_issue"] for e in many]),
            "payload_sampling": {"first": FIRST_PAYLOADS, "then_every": PAYLOAD_EVERY, "plus_newest": True},
        },
    }


def mb(b):
    return f"{b / 1e6:.1f} MB"


def tables(logs, errors):
    print("| Corpus | Lines | Raw B/line | Disk / raw | Disk B/line | One segment / raw | `values` | Peak extra disk | CPU µs/row | Peak RSS |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for r in logs:
        if r["days"] != 1:
            continue
        one = f"{r['one_segment_per_raw']:.2f}×" if "one_segment_per_raw" in r else ""
        extra = max(r["ingest_peak_extra_disk_bytes"], r.get("merge_peak_extra_disk_bytes", 0))
        values = r["share_of_index"].get("values", 0)
        print(f"| {r['corpus']} | {r['lines']:,} | {r['raw_bytes_per_line']:.0f} | {r['disk_per_raw']:.2f}× "
              f"| {r['disk_bytes_per_line']:.0f} | {one} | {values:.0%} | {mb(extra)} "
              f"| {r['ingest_cpu_us_per_row']:.1f} | {mb(r['ingest_peak_rss_bytes'])} |")
    print()
    for r in logs:
        if r["days"] == 1:
            continue
        s, ret = r["search"], r["retention"]
        print(f"{r['corpus']} over {r['days']} days: disk {mb(r['disk_bytes'])} ({r['disk_per_raw']:.2f}×), "
              f"RSS start {mb(s['search_rss_start_bytes'])}, 7d {mb(s['search_rss_7d_bytes'])}, "
              f"30d {mb(s['search_rss_30d_bytes'])}, 90d {mb(s['search_rss_90d_bytes'])}; "
              f"retention {ret['days_before']} -> {ret['days_after']} days freed {mb(ret['freed_bytes'])} "
              f"in {ret['secs']:.2f} s; peak ingest RSS {mb(r['ingest_peak_rss_bytes'])}")
    print()
    print("| Issues × events | SQLite B/event | Row B/event | Compacted row B/event | Payloads/issue | B/payload | B/hourly row | B/tag row | B/issue | CPU µs/event |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for e in errors:
        print(f"| {e['issues']:,} × {e['events_per_issue']:,} | {e['sqlite_bytes_per_event']:.0f} "
              f"| {e['row_bytes_per_event']:.0f} | {e['compacted_row_bytes_per_event']:.0f} | {e['payloads_per_issue']:.0f} "
              f"| {e['bytes_per_payload']:.0f} | {e['bytes_per_hourly_row']:.0f} "
              f"| {e['tag_bytes_per_row']:.0f} | {e['issue_bytes_per_issue']:.0f} | {e['ingest_cpu_us_per_event']:.0f} |")


def main():
    root, out, commit, shared = sys.argv[1:5]
    runs = read_runs(root)
    logs = [logs_row(r) for r in runs if r.get("kind") == "logs"]
    errors = [errors_row(r) for r in runs if r.get("kind") == "errors"]
    report = {
        "schema": SCHEMA, "version": VERSION,
        "measured": time.strftime("%Y-%m-%d"), "davy_commit": commit,
        "machine": machine(shared),
        "noisy": ["ingest_cpu_us_per_row", "ingest_cpu_us_per_event", "ingest_accept_rows_per_s",
                  "ingest_accept_events_per_s", "ingest_peak_rss_bytes", "search", "merge_secs",
                  "retention.secs"],
        "coefficients": coefficients(logs, errors),
        "logs": logs,
        "errors": errors,
    }
    tables(logs, errors)
    bad = [r["corpus"] for r in logs if not r["complete"]]
    bad += [f"errors {e['issues']}" for e in errors if not (e["complete"] and e["payloads_follow_sampling"])]
    if bad:
        print(f"incomplete runs, {out} not written: {bad}", file=sys.stderr)
        sys.exit(1)
    with open(out + ".part", "w") as f:
        json.dump(report, f, indent=2, ensure_ascii=False)
        f.write("\n")
    os.replace(out + ".part", out)


main()
