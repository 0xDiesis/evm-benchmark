#!/usr/bin/env python3
"""Build a tabular ledger from benchmark run directories.

The benchmark harness writes one report.json plus one meta.json per run. This
script joins those files and extracts the knobs and diagnostic counters that are
most useful when tuning performance.
"""

from __future__ import annotations

import argparse
import csv
import glob
import json
import os
import sys
from pathlib import Path
from typing import Any, Iterable


SCRIPT_DIR = Path(__file__).resolve().parent
RESULTS_DIR = SCRIPT_DIR.parent / "results"
RUNS_DIR = RESULTS_DIR / "runs"

GEO_MAX_RTT_MS = {
    "geo-eu": 90.0,
    "geo-us": 60.0,
    "geo-se-asia": 65.0,
    "geo-europe": 25.0,
    "geo-south-america": 105.0,
    "geo-global": 240.0,
    "geo-degraded": 200.0,
    "geo-intercontinental": 340.0,
}

DEFAULT_FIELDS = [
    "run",
    "timestamp",
    "chain",
    "mode",
    "env",
    "valid",
    "target_tps",
    "accepted_tps",
    "e2e_tps",
    "confirmed_pct",
    "p50_ms",
    "p95_ms",
    "p99_ms",
    "block_period",
    "ordering_window",
    "min_round_delay",
    "block_period_to_max_rtt",
    "max_proposal_txs",
    "max_proposal_bytes",
    "max_gas_per_proposal",
    "tx_selection_mode",
    "tx_source_lookahead_multiplier",
    "tx_source_max_scan",
    "tx_partition_fallback_fill",
    "fec_fanout",
    "fec_target_data_shreds",
    "dag_peer_command_channel_capacity",
    "referenced_payloads",
    "commit_unique_txs",
    "commit_duplicate_skips",
    "commit_capacity_skips",
    "commit_skip_pct",
    "tx_source_scanned",
    "tx_source_pulled",
    "tx_source_selected_primary",
    "tx_source_selected_fallback",
    "tx_source_scan_limit_hits",
    "tx_source_empty_selections",
    "tx_source_empty_backoff",
    "tx_source_empty_anomalous",
    "tx_source_proposal_utilization",
    "quic_fail_pct",
    "peer_command_full_drops",
    "fec_fail_per_1k",
    "missing_payload_deferrals",
    "payload_batch_requests",
    "payload_response_send_failures",
    "pending_vertices_after",
    "pending_parent_repairs",
    "fec_groups_expired",
    "fec_reassembly_hash_mismatches",
    "decode_failures",
    "txpool_pending_after",
    "txpool_basefee_after",
    "txpool_queued_after",
    "txpool_all_by_hash_after",
    "ordered_queue_depth_after",
    "invalid_reason",
    "path",
]


def read_json(path: Path) -> dict[str, Any]:
    with path.open("r", encoding="utf-8") as f:
        return json.load(f)


def parse_duration_ms(value: Any) -> float | None:
    if value is None:
        return None
    if isinstance(value, (int, float)):
        return float(value)
    text = str(value).strip()
    if not text:
        return None
    try:
        if text.endswith("ms"):
            return float(text[:-2])
        if text.endswith("s"):
            return float(text[:-1]) * 1000.0
        return float(text)
    except ValueError:
        return None


def nested(data: dict[str, Any], path: str, default: Any = None) -> Any:
    current: Any = data
    for part in path.split("."):
        if not isinstance(current, dict) or part not in current:
            return default
        current = current[part]
    return current


def counter_delta(report: dict[str, Any], name: str, default: float = 0.0) -> float:
    value = nested(report, f"prometheus_diagnostics.counters.{name}.delta")
    if value is None:
        return default
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


def gauge_after(report: dict[str, Any], name: str, default: float = 0.0) -> float:
    value = nested(report, f"prometheus_diagnostics.gauges.{name}.after")
    if value is None:
        value = nested(report, f"prometheus_diagnostics.gauges.{name}")
    if value is None:
        return default
    if isinstance(value, dict):
        value = value.get("value", value.get("after", default))
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


def report_paths(inputs: Iterable[str]) -> list[Path]:
    paths: list[Path] = []
    if inputs:
        for raw in inputs:
            path = Path(raw)
            if path.is_dir():
                candidate = path / "report.json"
                if candidate.is_file():
                    paths.append(candidate)
                else:
                    paths.extend(Path(p) for p in glob.glob(str(path / "**" / "report.json"), recursive=True))
            elif path.is_file():
                paths.append(path)
            else:
                raise SystemExit(f"not found: {raw}")
    else:
        for raw in glob.glob(str(RUNS_DIR / "*" / "*" / "*" / "report.json")):
            path = Path(raw)
            if "latest" not in path.parts:
                paths.append(path)

    deduped: dict[Path, Path] = {}
    for path in paths:
        deduped[path.resolve()] = path.resolve()
    return sorted(deduped.values())


def run_from_path(report_path: Path) -> dict[str, Any] | None:
    run_dir = report_path.parent
    meta_path = run_dir / "meta.json"
    try:
        report = read_json(report_path)
    except Exception as exc:
        print(f"WARN: skipping {report_path}: could not read report: {exc}", file=sys.stderr)
        return None
    try:
        meta = read_json(meta_path) if meta_path.is_file() else {}
    except Exception as exc:
        print(f"WARN: {meta_path}: could not read metadata: {exc}", file=sys.stderr)
        meta = {}

    parts = run_dir.relative_to(RUNS_DIR).parts if str(run_dir).startswith(str(RUNS_DIR)) else ()
    path_chain = parts[0] if len(parts) >= 1 else ""
    path_mode = parts[1] if len(parts) >= 2 else ""

    results = report.get("results", {})
    config = report.get("config", {})
    chain_config = meta.get("chain_config", {})
    bench_params = meta.get("bench_params", {})
    latency = results.get("latency", {})

    accepted = int(results.get("accepted", results.get("submitted", 0)) or 0)
    attempted = int(results.get("attempted", accepted) or 0)
    confirmed = int(results.get("confirmed", 0) or 0)
    failed = int(results.get("failed", max(0, attempted - accepted)) or 0)
    pending = int(results.get("pending", max(0, accepted - confirmed)) or 0)
    valid = bool(results.get("valid", failed == 0 and pending == 0 and confirmed == accepted))
    confirmed_pct = (confirmed / accepted * 100.0) if accepted else 0.0

    target_tps = bench_params.get("tps", config.get("target_tps", ""))
    duration = bench_params.get("duration", config.get("duration_secs", ""))
    accepted_tps = results.get("submitted_tps", "")
    e2e_tps = results.get("confirmed_tps", "")

    block_period = chain_config.get("block_period", "")
    ordering_window = chain_config.get("ordering_window", "")
    min_round_delay = chain_config.get("min_round_delay", "")
    block_ms = parse_duration_ms(block_period)
    env = meta.get("env") or ""
    max_rtt = GEO_MAX_RTT_MS.get(env)
    timing_margin = round(block_ms / max_rtt, 2) if block_ms and max_rtt else ""

    quic_success = counter_delta(report, "quic_send_succeeded")
    quic_failed = counter_delta(report, "quic_send_failed")
    quic_total = quic_success + quic_failed
    quic_fail_pct = round(quic_failed / quic_total * 100.0, 3) if quic_total else 0.0

    fec_shreds_sent = counter_delta(report, "fec_shreds_sent")
    fec_failures = counter_delta(report, "fec_shred_send_failures")
    fec_fail_per_1k = round(fec_failures / fec_shreds_sent * 1000.0, 3) if fec_shreds_sent else 0.0

    invalid_reason = results.get("invalid_reason", "")
    if not invalid_reason and not valid:
        if accepted and confirmed != accepted:
            invalid_reason = f"confirmed {confirmed} of {accepted}"
        elif failed:
            invalid_reason = f"{failed} failed submissions"
        elif pending:
            invalid_reason = f"{pending} pending"
        else:
            invalid_reason = "invalid result"

    commit_unique_txs = counter_delta(report, "commit_transactions_unique")
    commit_duplicate_skips = counter_delta(report, "commit_duplicate_skips")
    commit_capacity_skips = counter_delta(report, "commit_capacity_skips")
    commit_considered = commit_unique_txs + commit_duplicate_skips + commit_capacity_skips
    commit_skip_pct = (
        round((commit_duplicate_skips + commit_capacity_skips) / commit_considered * 100.0, 3)
        if commit_considered
        else 0.0
    )

    row = {
        "run": run_dir.name,
        "timestamp": meta.get("timestamp", ""),
        "chain": meta.get("chain", path_chain),
        "mode": meta.get("mode", path_mode),
        "env": env,
        "tag": meta.get("tag", ""),
        "valid": valid,
        "attempted": attempted,
        "accepted": accepted,
        "failed": failed,
        "confirmed": confirmed,
        "pending": pending,
        "confirmed_pct": round(confirmed_pct, 1),
        "target_tps": target_tps,
        "duration_secs": duration,
        "senders": bench_params.get("senders", ""),
        "batch_size": bench_params.get("batch_size", ""),
        "workers": bench_params.get("workers", ""),
        "accepted_tps": round(float(accepted_tps), 1) if accepted_tps != "" else "",
        "e2e_tps": round(float(e2e_tps), 1) if e2e_tps != "" else "",
        "p50_ms": latency.get("p50", 0),
        "p95_ms": latency.get("p95", 0),
        "p99_ms": latency.get("p99", 0),
        "latency_avg_ms": latency.get("avg", 0),
        "block_period": block_period,
        "ordering_window": ordering_window,
        "min_round_delay": min_round_delay,
        "leader_timeout": chain_config.get("leader_timeout", ""),
        "block_period_to_max_rtt": timing_margin,
        "max_proposal_txs": chain_config.get("max_proposal_txs", ""),
        "max_proposal_bytes": chain_config.get("max_proposal_bytes", ""),
        "max_gas_per_proposal": chain_config.get("max_gas_per_proposal", ""),
        "tx_selection_mode": chain_config.get("tx_selection_mode", ""),
        "tx_source_lookahead_multiplier": chain_config.get("tx_source_lookahead_multiplier", ""),
        "tx_source_max_scan": chain_config.get("tx_source_max_scan", ""),
        "tx_partition_fallback_fill": chain_config.get("tx_partition_fallback_fill", ""),
        "referenced_payloads": chain_config.get("referenced_payloads", ""),
        "fec_enabled": chain_config.get("fec_enabled", ""),
        "fec_fanout": chain_config.get("fec_max_concurrent_shred_sends_per_peer", ""),
        "fec_target_data_shreds": chain_config.get("fec_target_data_shreds", ""),
        "fec_redundancy_ratio": chain_config.get("fec_redundancy_ratio", ""),
        "dag_peer_command_channel_capacity": chain_config.get("dag_peer_command_channel_capacity", ""),
        "quic_hedge_rlpx_min_bytes": chain_config.get("quic_hedge_rlpx_min_bytes", ""),
        "quic_hedge_rlpx_broadcast_min_bytes": chain_config.get("quic_hedge_rlpx_broadcast_min_bytes", ""),
        "decode_failures": int(counter_delta(report, "decode_failed")),
        "dag_inbound_drops": int(counter_delta(report, "dag_inbound_dropped_full")),
        "peer_event_drops": int(counter_delta(report, "dag_peer_events_dropped_full")),
        "peer_command_full_drops": int(counter_delta(report, "dag_peer_commands_dropped_full")),
        "peer_command_closed_drops": int(counter_delta(report, "dag_peer_commands_dropped_closed")),
        "pending_vertex_drops": int(counter_delta(report, "pending_vertices_dropped_capacity")),
        "pending_parent_repairs": int(counter_delta(report, "pending_vertex_parent_repairs")),
        "pending_payload_repairs": int(counter_delta(report, "pending_vertex_payload_repairs")),
        "missing_payload_deferrals": int(counter_delta(report, "missing_payload_deferrals")),
        "payload_batch_requests": int(counter_delta(report, "payload_batch_requests_sent")),
        "payload_request_cooldown_skips": int(counter_delta(report, "payload_batch_request_skipped_recent")),
        "payload_response_send_failures": int(counter_delta(report, "payload_batch_response_send_failed")),
        "payload_response_rebroadcasts": int(counter_delta(report, "payload_batch_response_rebroadcast")),
        "commit_unique_txs": int(commit_unique_txs),
        "commit_duplicate_skips": int(commit_duplicate_skips),
        "commit_capacity_skips": int(commit_capacity_skips),
        "commit_skip_pct": commit_skip_pct,
        "tx_source_scanned": int(counter_delta(report, "tx_source_scanned")),
        "tx_source_pulled": int(counter_delta(report, "tx_source_pulled")),
        "tx_source_selected_primary": int(counter_delta(report, "tx_source_selected_primary")),
        "tx_source_selected_fallback": int(counter_delta(report, "tx_source_selected_fallback")),
        "tx_source_suppressed_committed": int(counter_delta(report, "tx_source_suppressed_committed")),
        "tx_source_suppressed_recently_proposed": int(
            counter_delta(report, "tx_source_suppressed_recently_proposed")
        ),
        "tx_source_skipped_partition": int(counter_delta(report, "tx_source_skipped_partition")),
        "tx_source_scan_limit_hits": int(counter_delta(report, "tx_source_scan_limit_hit")),
        "tx_source_empty_selections": int(counter_delta(report, "tx_source_empty_selection")),
        "tx_source_empty_backoff": int(counter_delta(report, "tx_source_empty_selection_backoff")),
        "tx_source_empty_anomalous": int(
            counter_delta(report, "tx_source_empty_selection_anomalous")
        ),
        "tx_source_proposal_utilization": round(
            float(gauge_after(report, "tx_source_proposal_utilization")), 4
        ),
        "ancestor_available_authors": round(float(gauge_after(report, "ancestor_available_authors")), 2),
        "ancestor_payload_unavailable_authors": round(float(gauge_after(report, "ancestor_payload_unavailable_authors")), 2),
        "ancestor_pending_unselectable_authors": round(float(gauge_after(report, "ancestor_pending_unselectable_authors")), 2),
        "pending_vertices_after": round(float(gauge_after(report, "pending_vertices")), 2),
        "pending_missing_parents_after": round(float(gauge_after(report, "pending_vertex_missing_parents")), 2),
        "pending_missing_payloads_after": round(float(gauge_after(report, "pending_vertex_missing_payloads")), 2),
        "parent_candidates": round(float(gauge_after(report, "parent_candidate_count")), 2),
        "parents_selected": round(float(gauge_after(report, "parent_selected_count")), 2),
        "parent_selected_power": round(float(gauge_after(report, "parent_selected_power")), 2),
        "fec_encodes": int(counter_delta(report, "fec_encode_count")),
        "fec_shreds_sent": int(fec_shreds_sent),
        "fec_shred_send_failures": int(fec_failures),
        "fec_groups_expired": int(counter_delta(report, "fec_groups_expired")),
        "fec_reassembly_hash_mismatches": int(
            counter_delta(report, "fec_reassembly_hash_mismatch")
        ),
        "fec_below_threshold_skips": int(counter_delta(report, "fec_below_threshold_skip")),
        "quic_send_successes": int(quic_success),
        "quic_send_failures": int(quic_failed),
        "quic_fail_pct": quic_fail_pct,
        "fec_fail_per_1k": fec_fail_per_1k,
        "quic_reconnect_attempts": int(counter_delta(report, "quic_reconnect_attempts")),
        "quic_reconnect_successes": int(counter_delta(report, "quic_reconnect_successes")),
        "quic_rlpx_disconnect_retained": int(counter_delta(report, "quic_rlpx_disconnect_retained")),
        "quic_hedged_rlpx": int(counter_delta(report, "quic_hedged_rlpx")),
        "quic_hedged_rlpx_broadcast": int(counter_delta(report, "quic_hedged_rlpx_broadcast")),
        "txpool_pending_after": int(gauge_after(report, "txpool_pending")),
        "txpool_basefee_after": int(gauge_after(report, "txpool_basefee")),
        "txpool_queued_after": int(gauge_after(report, "txpool_queued")),
        "txpool_all_by_hash_after": int(gauge_after(report, "txpool_all_by_hash")),
        "ordered_queue_depth_after": int(gauge_after(report, "ordered_queue_depth")),
        "publication_lag_after": int(gauge_after(report, "publication_lag")),
        "invalid_reason": invalid_reason,
        "path": str(run_dir),
    }
    return row


def filter_rows(rows: list[dict[str, Any]], args: argparse.Namespace) -> list[dict[str, Any]]:
    if args.chain:
        rows = [r for r in rows if str(r.get("chain")) == args.chain]
    if args.mode:
        rows = [r for r in rows if str(r.get("mode")) == args.mode]
    if args.env:
        rows = [r for r in rows if str(r.get("env")) == args.env]
    if args.only_valid:
        rows = [r for r in rows if bool(r.get("valid"))]

    sort_key = args.sort
    reverse = args.desc
    if sort_key:
        rows.sort(key=lambda r: sort_value(r.get(sort_key)), reverse=reverse)
    if args.limit:
        rows = rows[: args.limit]
    return rows


def sort_value(value: Any) -> tuple[int, Any]:
    if value in ("", None):
        return (3, "")
    if isinstance(value, bool):
        return (0, int(value))
    if isinstance(value, (int, float)):
        return (0, value)
    try:
        return (0, float(value))
    except (TypeError, ValueError):
        return (1, str(value))


def selected_fields(args: argparse.Namespace) -> list[str]:
    if args.fields:
        return [field.strip() for field in args.fields.split(",") if field.strip()]
    return DEFAULT_FIELDS


def format_value(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if value is None:
        return ""
    return str(value)


def render_csv(rows: list[dict[str, Any]], fields: list[str]) -> str:
    from io import StringIO

    output = StringIO()
    writer = csv.DictWriter(output, fieldnames=fields, extrasaction="ignore")
    writer.writeheader()
    for row in rows:
        writer.writerow({field: row.get(field, "") for field in fields})
    return output.getvalue()


def render_json(rows: list[dict[str, Any]], fields: list[str]) -> str:
    trimmed = [{field: row.get(field, "") for field in fields} for row in rows]
    return json.dumps(trimmed, indent=2) + "\n"


def render_markdown(rows: list[dict[str, Any]], fields: list[str]) -> str:
    lines = []
    lines.append("| " + " | ".join(fields) + " |")
    lines.append("| " + " | ".join("---" for _ in fields) + " |")
    for row in rows:
        values = [format_value(row.get(field, "")).replace("|", "\\|") for field in fields]
        lines.append("| " + " | ".join(values) + " |")
    return "\n".join(lines) + "\n"


def render_table(rows: list[dict[str, Any]], fields: list[str]) -> str:
    if not rows:
        return "No runs found.\n"
    widths = {field: len(field) for field in fields}
    for row in rows:
        for field in fields:
            widths[field] = min(max(widths[field], len(format_value(row.get(field, "")))), 42)

    def cell(field: str, value: Any) -> str:
        text = format_value(value)
        if len(text) > widths[field]:
            text = text[: max(0, widths[field] - 1)] + "..."
        return text.ljust(widths[field])

    header = "  ".join(field.ljust(widths[field]) for field in fields)
    rule = "  ".join("-" * widths[field] for field in fields)
    lines = [header, rule]
    for row in rows:
        lines.append("  ".join(cell(field, row.get(field, "")) for field in fields))
    return "\n".join(lines) + "\n"


def render(rows: list[dict[str, Any]], fields: list[str], output_format: str) -> str:
    if output_format == "csv":
        return render_csv(rows, fields)
    if output_format == "json":
        return render_json(rows, fields)
    if output_format == "md":
        return render_markdown(rows, fields)
    return render_table(rows, fields)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="*", help="Run directories or report.json paths. Defaults to all run results.")
    parser.add_argument("--chain", help="Filter by chain.")
    parser.add_argument("--mode", help="Filter by execution mode.")
    parser.add_argument("--env", help="Filter by environment.")
    parser.add_argument("--only-valid", action="store_true", help="Keep only valid runs.")
    parser.add_argument("--limit", type=int, default=0, help="Limit rows after sorting/filtering.")
    parser.add_argument("--sort", default="timestamp", help="Sort key. Use any output field name.")
    parser.add_argument("--desc", action="store_true", help="Sort descending.")
    parser.add_argument("--fields", help="Comma-separated output fields.")
    parser.add_argument("--format", choices=("table", "csv", "json", "md"), default="table")
    parser.add_argument("--out", help="Write output to this file instead of stdout.")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    rows = [row for path in report_paths(args.paths) if (row := run_from_path(path))]
    rows = filter_rows(rows, args)
    fields = selected_fields(args)
    output = render(rows, fields, args.format)

    if args.out:
        out_path = Path(args.out)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(output, encoding="utf-8")
        print(f"Wrote {len(rows)} rows to {out_path}")
    else:
        print(output, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
