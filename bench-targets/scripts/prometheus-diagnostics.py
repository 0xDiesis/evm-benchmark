#!/usr/bin/env python3
"""Capture benchmark-focused Prometheus snapshots and deltas."""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any


COUNTERS: dict[str, str] = {
    "dag_inbound_dropped_full": "sum(reth_diesis_dag_inbound_dropped_full_total)",
    "dag_inbound_burst_drained": "sum(reth_diesis_dag_inbound_burst_drained_total)",
    "dag_peer_events_dropped_full": "sum(reth_diesis_dag_peer_events_dropped_full_total)",
    "dag_peer_commands_dropped_full": "sum(reth_diesis_dag_peer_command_dropped_full_total)",
    "dag_peer_commands_dropped_closed": "sum(reth_diesis_dag_peer_command_dropped_closed_total)",
    "pending_vertices_dropped_capacity": "sum(reth_diesis_consensus_pending_vertices_dropped_capacity_total)",
    "pending_vertices_dropped_bytes": "sum(reth_diesis_consensus_pending_vertices_dropped_bytes_total)",
    "pending_vertex_parent_repairs": "sum(reth_diesis_consensus_pending_vertex_parent_repairs_total)",
    "pending_vertex_payload_repairs": "sum(reth_diesis_consensus_pending_vertex_payload_repairs_total)",
    "missing_payload_deferrals": "sum(reth_diesis_consensus_vertex_deferred_missing_payload_total)",
    "decode_failed": "sum(reth_diesis_consensus_decode_failed_total)",
    "round_ahead_rejections": "sum(reth_diesis_consensus_vertex_rejected_round_ahead_total)",
    "round_ahead_catchups": "sum(reth_diesis_consensus_vertex_round_ahead_catchup_total)",
    "payload_ref_rejections": "sum(reth_diesis_consensus_vertex_rejected_payload_ref_total)",
    "payload_batch_requests_sent": "sum(reth_diesis_consensus_payload_batch_requests_sent_total)",
    "payload_batch_request_skipped_recent": "sum(reth_diesis_consensus_payload_batch_request_skipped_recent_total)",
    "payload_batch_request_cooldown_pruned": "sum(reth_diesis_consensus_payload_batch_request_cooldown_pruned_total)",
    "payload_batch_request_author_targets": "sum(reth_diesis_consensus_payload_batch_request_author_targets_total)",
    "payload_batch_response_send_failed": "sum(reth_diesis_consensus_payload_batch_response_send_failed_total)",
    "payload_batch_response_rebroadcast": "sum(reth_diesis_consensus_payload_batch_response_rebroadcast_total)",
    "payload_batch_response_rebroadcast_failed": "sum(reth_diesis_consensus_payload_batch_response_rebroadcast_failed_total)",
    "payload_batch_response_rebroadcast_skipped_recent": "sum(reth_diesis_consensus_payload_batch_response_rebroadcast_skipped_recent_total)",
    "payload_batch_response_rebroadcast_cooldown_pruned": "sum(reth_diesis_consensus_payload_batch_response_rebroadcast_cooldown_pruned_total)",
    "payload_batches_stored": "sum(reth_diesis_consensus_payload_batches_stored_total)",
    "payload_batches_broadcast": "sum(reth_diesis_consensus_payload_batches_broadcast_total)",
    "payload_batch_author_rejections": "sum(reth_diesis_consensus_payload_batch_rejected_author_total)",
    "payload_batch_invalid_rejections": "sum(reth_diesis_consensus_payload_batch_rejected_invalid_total)",
    "referenced_proposals_created": "sum(reth_diesis_consensus_referenced_proposals_created_total)",
    "commit_missing_payloads": "sum(reth_diesis_commit_proposal_missing_payload_total)",
    "commit_transactions_skipped": "sum(reth_diesis_commit_proposal_transactions_skipped_total)",
    "txs_executed": "sum(reth_diesis_txs_executed)",
    "blocks_executed": "sum(reth_diesis_blocks_executed)",
    "pipeline_blocks_executed": "sum(reth_diesis_pipeline_blocks_executed)",
    "pipeline_blocks_finalized": "sum(reth_diesis_pipeline_blocks_finalized)",
    "pipeline_blocks_published": "sum(reth_diesis_pipeline_blocks_published)",
    "txpool_inserted": "sum(reth_transaction_pool_inserted_transactions)",
    "txpool_invalid": "sum(reth_transaction_pool_invalid_transactions)",
    "txpool_pending_evicted": "sum(reth_transaction_pool_pending_transactions_evicted)",
    "rpc_rate_limited": "sum(reth_diesis_rpc_tx_rate_limited_total)",
    "fec_encode_count": "sum(reth_diesis_fec_encode_count)",
    "fec_shreds_total": "sum(reth_diesis_fec_shreds_total)",
    "fec_shreds_sent": "sum(reth_diesis_fec_shreds_sent)",
    "fec_shreds_received": "sum(reth_diesis_fec_shreds_received)",
    "fec_shreds_rejected_invalid": "sum(reth_diesis_fec_shreds_rejected_invalid)",
    "fec_shreds_rejected_inconsistent": "sum(reth_diesis_fec_shreds_rejected_inconsistent)",
    "fec_groups_completed": "sum(reth_diesis_fec_groups_completed)",
    "fec_groups_expired": "sum(reth_diesis_fec_groups_expired)",
    "fec_groups_dropped_capacity": "sum(reth_diesis_fec_groups_dropped_capacity)",
    "fec_no_peers_skip": "sum(reth_diesis_fec_no_peers_skip)",
    "fec_below_threshold_skip": "sum(reth_diesis_fec_below_threshold_skip)",
    "fec_invalid_config_skip": "sum(reth_diesis_fec_invalid_config_skip)",
    "fec_oversized_skip": "sum(reth_diesis_fec_oversized_skip)",
    "fec_skip_count": "sum(reth_diesis_fec_skip_count)",
    "fec_shred_send_failures": "sum(reth_diesis_fec_shred_send_failures)",
    "fec_reassembly_complete": "sum(reth_diesis_fec_reassembly_complete)",
    "fec_reassembly_duplicate": "sum(reth_diesis_fec_reassembly_duplicate)",
    "fec_reassembly_evicted": "sum(reth_diesis_fec_reassembly_evicted)",
    "fec_reassembly_hash_mismatch": "sum(reth_diesis_fec_reassembly_hash_mismatch)",
    "quic_connect_failures": "sum(reth_diesis_quic_connect_failures)",
    "quic_send_succeeded": "sum(reth_diesis_quic_send_succeeded_total)",
    "quic_send_failed": "sum(reth_diesis_quic_send_failed_total)",
    "quic_reconnect_attempts": "sum(reth_diesis_quic_reconnect_attempts)",
    "quic_reconnect_successes": "sum(reth_diesis_quic_reconnect_successes)",
    "quic_rlpx_disconnect_retained": "sum(reth_diesis_quic_rlpx_disconnect_retained_total)",
    "quic_advertisement_rejected_nonvalidator": "sum(reth_diesis_quic_advertisement_rejected_nonvalidator_total)",
    "quic_broadcast_dedup": "sum(reth_diesis_quic_broadcast_dedup_count)",
    "quic_fallback": "sum(reth_diesis_quic_fallback_count)",
    "quic_hedged_rlpx": "sum(reth_diesis_quic_hedged_rlpx_total)",
    "quic_hedged_rlpx_failed": "sum(reth_diesis_quic_hedged_rlpx_failed_total)",
    "quic_hedged_rlpx_broadcast": "sum(reth_diesis_quic_hedged_rlpx_broadcast_total)",
}

SERIES_COUNTERS: dict[str, str] = {
    "decode_failed_by_msg_id": (
        "sum by (msg_id) (reth_diesis_consensus_decode_failed_total)"
    ),
    "payload_batch_response_send_failed_by_role": (
        "sum by (requester_is_validator) "
        "(reth_diesis_consensus_payload_batch_response_send_failed_total)"
    ),
    "payload_batch_response_rebroadcast_skipped_by_role": (
        "sum by (requester_is_validator) "
        "(reth_diesis_consensus_payload_batch_response_rebroadcast_skipped_recent_total)"
    ),
    "quic_send_succeeded_by_op_msg": (
        "sum by (op,msg_id) (reth_diesis_quic_send_succeeded_total)"
    ),
    "quic_send_failed_by_op_msg": (
        "sum by (op,msg_id) (reth_diesis_quic_send_failed_total)"
    ),
    "quic_hedged_rlpx_by_msg_id": (
        "sum by (msg_id) (reth_diesis_quic_hedged_rlpx_total)"
    ),
    "quic_hedged_rlpx_broadcast_by_msg_id": (
        "sum by (msg_id) (reth_diesis_quic_hedged_rlpx_broadcast_total)"
    ),
}

GAUGES: dict[str, str] = {
    "txpool_pending": "sum(reth_transaction_pool_pending_pool_transactions)",
    "txpool_basefee": "sum(reth_transaction_pool_basefee_pool_transactions)",
    "txpool_queued": "sum(reth_transaction_pool_queued_pool_transactions)",
    "txpool_total": "sum(reth_transaction_pool_total_transactions)",
    "txpool_all_by_hash": "sum(reth_transaction_pool_all_transactions_by_hash)",
    "txpool_all_by_sender": "sum(reth_transaction_pool_all_transactions_by_all_senders)",
    "consensus_current_round": "max(reth_diesis_consensus_current_round)",
    "consensus_head": "max(reth_diesis_consensus_head)",
    "ordered_queue_depth": "sum(reth_diesis_pipeline_ordered_queue_depth)",
    "executed_queue_depth": "sum(reth_diesis_pipeline_executed_queue_depth)",
    "execution_head": "max(reth_diesis_execution_head)",
    "publication_head": "max(reth_diesis_publication_head)",
    "persisted_head": "max(reth_diesis_pipeline_persisted_head)",
    "publication_lag": "max(reth_diesis_pipeline_publication_lag)",
    "persistence_lag": "max(reth_diesis_pipeline_persistence_lag)",
    "overlay_depth": "sum(reth_diesis_pipeline_overlay_depth)",
    "verkle_stash_depth": "sum(reth_diesis_pipeline_verkle_stash_depth)",
    "witness_stash_depth": "sum(reth_diesis_pipeline_witness_stash_depth)",
    "execution_view_stash_depth": "sum(reth_diesis_pipeline_execution_view_stash_depth)",
    "payload_batch_last_tx_count": "max(reth_diesis_consensus_payload_batch_last_tx_count)",
    "payload_batch_last_bytes": "max(reth_diesis_consensus_payload_batch_last_bytes)",
    "ancestor_available_authors": "avg(reth_diesis_consensus_ancestor_available_authors)",
    "ancestor_eligible_authors": "avg(reth_diesis_consensus_ancestor_eligible_authors)",
    "ancestor_payload_unavailable_authors": "avg(reth_diesis_consensus_ancestor_payload_unavailable_authors)",
    "ancestor_pending_unselectable_authors": "avg(reth_diesis_consensus_ancestor_pending_unselectable_authors)",
    "pending_vertices": "sum(reth_diesis_consensus_pending_vertices)",
    "pending_vertex_missing_parents": "sum(reth_diesis_consensus_pending_vertex_missing_parents)",
    "pending_vertex_missing_payloads": "sum(reth_diesis_consensus_pending_vertex_missing_payloads)",
    "parent_candidate_count": "avg(reth_diesis_consensus_parent_candidate_count)",
    "parent_selected_count": "avg(reth_diesis_consensus_parent_selected_count)",
    "parent_selected_power": "avg(reth_diesis_consensus_parent_selected_power)",
    "fec_enabled": "max(reth_diesis_fec_enabled)",
    "fec_redundancy_ratio": "max(reth_diesis_fec_redundancy_ratio)",
    "fec_min_message_size": "max(reth_diesis_fec_min_message_size)",
    "fec_target_data_shreds": "max(reth_diesis_fec_target_data_shreds)",
    "fec_max_concurrent_shred_sends_per_peer": "max(reth_diesis_fec_max_concurrent_shred_sends_per_peer)",
    "dag_peer_command_channel_capacity": "max(reth_diesis_dag_peer_command_channel_capacity)",
}

HISTOGRAMS: dict[str, str] = {
    "block_execution_ms": "reth_diesis_block_execution_ms",
    "tx_avg_execution_us": "reth_diesis_tx_avg_execution_us",
    "handoff_queue_latency_ms": "reth_diesis_handoff_queue_latency_ms",
    "ordered_queue_wait_ms": "reth_diesis_pipeline_ordered_queue_wait_ms",
    "executed_queue_wait_ms": "reth_diesis_pipeline_executed_queue_wait_ms",
    "pipeline_execution_ms": "reth_diesis_pipeline_execution_ms",
    "pipeline_state_root_ms": "reth_diesis_pipeline_state_root_ms",
    "pipeline_finalize_total_ms": "reth_diesis_pipeline_finalize_total_ms",
    "pipeline_publication_total_ms": "reth_diesis_pipeline_publication_total_ms",
    "pipeline_publication_new_payload_ms": "reth_diesis_pipeline_publication_new_payload_ms",
    "pipeline_publication_fcu_ms": "reth_diesis_pipeline_publication_fcu_ms",
    "pipeline_block_budget_ms": "reth_diesis_pipeline_block_budget_ms",
    "fec_payload_bytes": "reth_diesis_fec_payload_bytes",
    "fec_encode_duration": "reth_diesis_fec_encode_duration",
    "fec_repair_shreds_used": "reth_diesis_fec_repair_shreds_used",
}

HISTOGRAM_QUERIES: dict[str, str] = {}
for name, metric in HISTOGRAMS.items():
    HISTOGRAM_QUERIES[f"{name}_sum"] = f"sum({metric}_sum)"
    HISTOGRAM_QUERIES[f"{name}_count"] = f"sum({metric}_count)"

ALL_QUERIES = {**COUNTERS, **GAUGES, **HISTOGRAM_QUERIES}


def query_prometheus(base_url: str, query: str, timeout: float) -> float | None:
    url = base_url.rstrip("/") + "/api/v1/query?" + urllib.parse.urlencode({"query": query})
    request = urllib.request.Request(url, headers={"Accept": "application/json"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        payload = json.loads(response.read().decode("utf-8"))
    if payload.get("status") != "success":
        return None
    result = payload.get("data", {}).get("result", [])
    if not result:
        return 0.0
    value = result[0].get("value", [None, None])[1]
    if value is None:
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def query_prometheus_vector(base_url: str, query: str, timeout: float) -> list[dict[str, Any]]:
    url = base_url.rstrip("/") + "/api/v1/query?" + urllib.parse.urlencode({"query": query})
    request = urllib.request.Request(url, headers={"Accept": "application/json"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        payload = json.loads(response.read().decode("utf-8"))
    if payload.get("status") != "success":
        return []

    output = []
    for item in payload.get("data", {}).get("result", []):
        value = numeric(item.get("value", [None, None])[1])
        if value is None:
            continue
        labels = {
            key: str(label_value)
            for key, label_value in item.get("metric", {}).items()
            if key != "__name__"
        }
        output.append({"labels": labels, "value": value})
    output.sort(key=lambda row: sorted(row["labels"].items()))
    return output


def snapshot(args: argparse.Namespace) -> int:
    values: dict[str, float | None] = {}
    series_values: dict[str, list[dict[str, Any]]] = {}
    errors: dict[str, str] = {}
    for name, query in ALL_QUERIES.items():
        try:
            values[name] = query_prometheus(args.url, query, args.timeout)
        except Exception as exc:  # noqa: BLE001 - diagnostics should not fail benchmarks.
            values[name] = None
            errors[name] = str(exc)

    for name, query in SERIES_COUNTERS.items():
        try:
            series_values[name] = query_prometheus_vector(args.url, query, args.timeout)
        except Exception as exc:  # noqa: BLE001 - diagnostics should not fail benchmarks.
            series_values[name] = []
            errors[f"series:{name}"] = str(exc)

    output = {
        "captured_at_unix": time.time(),
        "url": args.url,
        "queries": ALL_QUERIES,
        "series_queries": SERIES_COUNTERS,
        "counters": sorted(COUNTERS),
        "series_counters": sorted(SERIES_COUNTERS),
        "gauges": sorted(GAUGES),
        "histograms": sorted(HISTOGRAMS),
        "values": values,
        "series_values": series_values,
        "errors": errors,
    }
    Path(args.out).write_text(json.dumps(output, indent=2) + "\n")
    return 0


def numeric(value: Any) -> float | None:
    if value is None:
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def load_json(path: str) -> dict[str, Any]:
    return json.loads(Path(path).read_text())


def compute_delta(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    before_values = before.get("values", {})
    after_values = after.get("values", {})
    counters: dict[str, dict[str, float | None]] = {}
    series_counters: dict[str, list[dict[str, Any]]] = {}
    gauges: dict[str, dict[str, float | None]] = {}
    histograms: dict[str, dict[str, float | None]] = {}

    for name in COUNTERS:
        start = numeric(before_values.get(name))
        end = numeric(after_values.get(name))
        delta = None if start is None or end is None else max(0.0, end - start)
        counters[name] = {"before": start, "after": end, "delta": delta}

    for name in GAUGES:
        start = numeric(before_values.get(name))
        end = numeric(after_values.get(name))
        change = None if start is None or end is None else end - start
        gauges[name] = {"before": start, "after": end, "change": change}

    before_series = before.get("series_values", {})
    after_series = after.get("series_values", {})
    for name in SERIES_COUNTERS:
        series_counters[name] = compute_series_counter_delta(
            before_series.get(name, []), after_series.get(name, [])
        )

    for name in HISTOGRAMS:
        sum_start = numeric(before_values.get(f"{name}_sum"))
        sum_end = numeric(after_values.get(f"{name}_sum"))
        count_start = numeric(before_values.get(f"{name}_count"))
        count_end = numeric(after_values.get(f"{name}_count"))
        sum_delta = None if sum_start is None or sum_end is None else max(0.0, sum_end - sum_start)
        count_delta = (
            None if count_start is None or count_end is None else max(0.0, count_end - count_start)
        )
        avg = (
            None
            if sum_delta is None or count_delta is None or count_delta <= 0.0
            else sum_delta / count_delta
        )
        histograms[name] = {
            "sum_before": sum_start,
            "sum_after": sum_end,
            "sum_delta": sum_delta,
            "count_before": count_start,
            "count_after": count_end,
            "count_delta": count_delta,
            "avg": avg,
        }

    return {
        "captured_at_unix": time.time(),
        "before_captured_at_unix": before.get("captured_at_unix"),
        "after_captured_at_unix": after.get("captured_at_unix"),
        "url": after.get("url") or before.get("url"),
        "counters": counters,
        "series_counters": series_counters,
        "gauges": gauges,
        "histograms": histograms,
        "errors": {
            "before": before.get("errors", {}),
            "after": after.get("errors", {}),
        },
    }


def labels_key(labels: dict[str, str]) -> str:
    return json.dumps(sorted(labels.items()), separators=(",", ":"))


def compute_series_counter_delta(
    before_rows: list[dict[str, Any]], after_rows: list[dict[str, Any]]
) -> list[dict[str, Any]]:
    before_by_key = {labels_key(row.get("labels", {})): row for row in before_rows}
    after_by_key = {labels_key(row.get("labels", {})): row for row in after_rows}
    output = []
    for key in set(before_by_key) | set(after_by_key):
        before_value = numeric(before_by_key.get(key, {}).get("value")) or 0.0
        after_value = numeric(after_by_key.get(key, {}).get("value")) or 0.0
        labels = after_by_key.get(key, before_by_key.get(key, {})).get("labels", {})
        output.append(
            {
                "labels": labels,
                "before": before_value,
                "after": after_value,
                "delta": max(0.0, after_value - before_value),
            }
        )
    output.sort(key=lambda row: (-float(row.get("delta") or 0.0), sorted(row["labels"].items())))
    return output


def delta(args: argparse.Namespace) -> int:
    before = load_json(args.before)
    after = load_json(args.after)
    output = compute_delta(before, after)
    Path(args.out).write_text(json.dumps(output, indent=2) + "\n")

    if args.report:
        report_path = Path(args.report)
        if report_path.exists():
            report = load_json(str(report_path))
            report["prometheus_diagnostics"] = output
            report_path.write_text(json.dumps(report, indent=2) + "\n")

    return 0


def print_summary(args: argparse.Namespace) -> int:
    data = load_json(args.delta)
    counters = data.get("counters", {})
    gauges = data.get("gauges", {})
    histograms = data.get("histograms", {})
    important = [
        ("DAG inbound drops", counters.get("dag_inbound_dropped_full", {}).get("delta")),
        ("Peer event drops", counters.get("dag_peer_events_dropped_full", {}).get("delta")),
        ("Peer command full drops", counters.get("dag_peer_commands_dropped_full", {}).get("delta")),
        ("Peer command closed drops", counters.get("dag_peer_commands_dropped_closed", {}).get("delta")),
        ("Pending vertex drops", counters.get("pending_vertices_dropped_capacity", {}).get("delta")),
        ("Pending parent repairs", counters.get("pending_vertex_parent_repairs", {}).get("delta")),
        ("Pending payload repairs", counters.get("pending_vertex_payload_repairs", {}).get("delta")),
        ("Decode failures", counters.get("decode_failed", {}).get("delta")),
        ("Missing payload deferrals", counters.get("missing_payload_deferrals", {}).get("delta")),
        ("Payload batch requests", counters.get("payload_batch_requests_sent", {}).get("delta")),
        (
            "Payload request cooldown skips",
            counters.get("payload_batch_request_skipped_recent", {}).get("delta"),
        ),
        (
            "Payload request cooldown pruned",
            counters.get("payload_batch_request_cooldown_pruned", {}).get("delta"),
        ),
        (
            "Payload response send failures",
            counters.get("payload_batch_response_send_failed", {}).get("delta"),
        ),
        (
            "Payload response rebroadcasts",
            counters.get("payload_batch_response_rebroadcast", {}).get("delta"),
        ),
        (
            "Payload response rebroadcast cooldown skips",
            counters.get("payload_batch_response_rebroadcast_skipped_recent", {}).get("delta"),
        ),
        ("Ancestor available authors", gauges.get("ancestor_available_authors", {}).get("after")),
        (
            "Ancestor payload-unavailable authors",
            gauges.get("ancestor_payload_unavailable_authors", {}).get("after"),
        ),
        (
            "Ancestor pending-unselectable authors",
            gauges.get("ancestor_pending_unselectable_authors", {}).get("after"),
        ),
        ("Pending vertices", gauges.get("pending_vertices", {}).get("after")),
        (
            "Pending vertex missing parents",
            gauges.get("pending_vertex_missing_parents", {}).get("after"),
        ),
        (
            "Pending vertex missing payloads",
            gauges.get("pending_vertex_missing_payloads", {}).get("after"),
        ),
        ("Parent candidates", gauges.get("parent_candidate_count", {}).get("after")),
        ("Parents selected", gauges.get("parent_selected_count", {}).get("after")),
        ("Parent selected power", gauges.get("parent_selected_power", {}).get("after")),
        ("FEC encodes", counters.get("fec_encode_count", {}).get("delta")),
        ("FEC shreds sent", counters.get("fec_shreds_sent", {}).get("delta")),
        (
            "FEC invalid shreds rejected",
            counters.get("fec_shreds_rejected_invalid", {}).get("delta"),
        ),
        (
            "FEC inconsistent shreds rejected",
            counters.get("fec_shreds_rejected_inconsistent", {}).get("delta"),
        ),
        ("FEC shred send failures", counters.get("fec_shred_send_failures", {}).get("delta")),
        (
            "FEC reassembly hash mismatches",
            counters.get("fec_reassembly_hash_mismatch", {}).get("delta"),
        ),
        ("FEC groups expired", counters.get("fec_groups_expired", {}).get("delta")),
        ("FEC below-threshold skips", counters.get("fec_below_threshold_skip", {}).get("delta")),
        ("QUIC send successes", counters.get("quic_send_succeeded", {}).get("delta")),
        ("QUIC send failures", counters.get("quic_send_failed", {}).get("delta")),
        ("QUIC reconnect attempts", counters.get("quic_reconnect_attempts", {}).get("delta")),
        ("QUIC reconnect successes", counters.get("quic_reconnect_successes", {}).get("delta")),
        ("QUIC routes retained after RLPx disconnect", counters.get("quic_rlpx_disconnect_retained", {}).get("delta")),
        ("QUIC to RLPx hedges", counters.get("quic_hedged_rlpx", {}).get("delta")),
        (
            "QUIC broadcast RLPx hedges",
            counters.get("quic_hedged_rlpx_broadcast", {}).get("delta"),
        ),
        ("Txpool pending after", gauges.get("txpool_pending", {}).get("after")),
        ("Txpool basefee after", gauges.get("txpool_basefee", {}).get("after")),
        ("Txpool queued after", gauges.get("txpool_queued", {}).get("after")),
        ("Txpool all by hash after", gauges.get("txpool_all_by_hash", {}).get("after")),
        ("Ordered queue depth after", gauges.get("ordered_queue_depth", {}).get("after")),
        ("Publication lag after", gauges.get("publication_lag", {}).get("after")),
        ("Avg pipeline execution ms", histograms.get("pipeline_execution_ms", {}).get("avg")),
        ("Avg finalize ms", histograms.get("pipeline_finalize_total_ms", {}).get("avg")),
        ("Avg publication ms", histograms.get("pipeline_publication_total_ms", {}).get("avg")),
    ]
    for label, value in important:
        if value is None:
            display = "n/a"
        elif abs(value - round(value)) < 0.000001:
            display = f"{int(round(value))}"
        else:
            display = f"{value:.3f}"
        print(f"  {label}: {display}")

    series = data.get("series_counters", {})
    for series_name, label in [
        ("quic_send_failed_by_op_msg", "Top QUIC send failure labels"),
        ("decode_failed_by_msg_id", "Top decode failure labels"),
    ]:
        rows = [
            row for row in series.get(series_name, [])
            if float(row.get("delta") or 0.0) > 0.0
        ][:3]
        if not rows:
            continue
        formatted = []
        for row in rows:
            labels = ",".join(f"{key}={value}" for key, value in sorted(row["labels"].items()))
            formatted.append(f"{labels}:{int(round(row['delta']))}")
        print(f"  {label}: {'; '.join(formatted)}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    snapshot_parser = sub.add_parser("snapshot")
    snapshot_parser.add_argument("--url", required=True)
    snapshot_parser.add_argument("--out", required=True)
    snapshot_parser.add_argument("--timeout", type=float, default=2.0)
    snapshot_parser.set_defaults(func=snapshot)

    delta_parser = sub.add_parser("delta")
    delta_parser.add_argument("--before", required=True)
    delta_parser.add_argument("--after", required=True)
    delta_parser.add_argument("--out", required=True)
    delta_parser.add_argument("--report")
    delta_parser.set_defaults(func=delta)

    summary_parser = sub.add_parser("summary")
    summary_parser.add_argument("--delta", required=True)
    summary_parser.set_defaults(func=print_summary)

    args = parser.parse_args()
    return int(args.func(args))


if __name__ == "__main__":
    sys.exit(main())
