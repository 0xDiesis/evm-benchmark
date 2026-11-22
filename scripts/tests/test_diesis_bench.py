#!/usr/bin/env python3
"""Offline tests for the Diesis fleet, observer and A/B runner (no Docker, no chain)."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import threading
import types
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


def load(path):
    spec = importlib.util.spec_from_file_location(path.stem.replace("-", "_"), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


observer = load(ROOT / "bench-targets/scripts/diesis-observer.py")
fleet = load(ROOT / "bench-targets/chains/diesis/fleet.py")
ab = load(ROOT / "bench-targets/scripts/ab.py")
judged = load(ROOT / "bench-targets/scripts/judged.py")
idle = load(ROOT / "bench-targets/scripts/diesis-idle.py")
timeline = load(ROOT / "bench-targets/scripts/diesis-timeline.py")

W0 = 1.795e9


def prom(k, pending=100):
    return {("reth_sync_checkpoint", '{stage="Finish"}'): 10 * k,
            ("reth_diesis_horizon_committed_transactions_total", ""): 9000 * k,
            ("reth_diesis_consensus_current_round", ""): 30 * k,
            ("reth_diesis_lock_hold_ms_sum", '{lock="selected_owner",site="a"}'): 3000 * k,
            ("reth_diesis_lock_hold_ms_sum", '{lock="selected_owner",site="b"}'): 1000 * k,
            ("reth_diesis_lock_hold_ms_sum", '{lock="other",site="c"}'): 99999 * k,
            ("reth_diesis_mpt_certify_phase_ms_sum", '{phase="quorum_wait"}'): 2000 * k,
            ("reth_diesis_mpt_certify_phase_ms", '{phase="quorum_wait",quantile="0.99"}'): 600.0,
            ("reth_transaction_pool_pending_pool_transactions", ""): pending * k,
            ("reth_diesis_commit_proposal_duplicate_skipped_total", ""): 20 * k,
            ("reth_diesis_producer_output_deferred_transactions_total", ""): 50 * k,
            ("reth_diesis_outcome_fallback_entries_total", '{cause="carrier_round_below_quorum"}'): k,
            ("reth_diesis_checkpoint_control_inbound_rejected_total", '{reason="lane_full"}'): 2 * k}


def synthetic(tmp, pending=100, unfinalized_from=None):
    """120 blocks 0.5 s apart from W0, 1,000 txs each (every 10th empty), finalized 1 s after first
    seen; each tx submitted 2 s before its block was seen; one hash never included. Load starts
    at W0 - 2 (first submission), so warmup 7 s and duration 57 s give the window [W0+5, W0+55]."""
    obs = types.SimpleNamespace(lock=threading.Lock(), blocks={}, samples=[], exited=[], errors=[], heads=[])
    rows = []
    for n in range(120):
        seen = W0 + n * 0.5
        hashes = [] if n % 10 == 0 else [f"0x{n:04x}{i:060x}" for i in range(1000)]
        block = dict(seen=seen, hashes=hashes, finalized=seen + 1, all4=seen + 0.2)
        if unfinalized_from is not None and n >= unfinalized_from:
            block.pop("finalized")
        obs.blocks[n] = block
        rows += [(h, int((seen - 2) * 1e6)) for h in hashes]
    rows.insert(0, ("0xfirst", int((W0 - 2) * 1e6)))
    rows.append(("0xdead", int((W0 + 20) * 1e6)))
    for k in range(14):
        obs.samples.append((W0 + k * 5, [prom(k, pending)] * 4, {f"n{i}": int(k * 5e9) for i in range(4)}, 4.0,
                            {f"n{i}": (i + 1) * 1000 + k for i in range(4)},
                            {f"n{i}": (int(k * 5e8), k * 10, k * 50) for i in range(4)}))
    for k in range(600):
        lead = 100 + k // 10
        obs.heads.append((W0 + k * 0.1, [lead, lead, lead - (k % 3), lead], []))
    log = Path(tmp) / "submit-log.csv"
    log.write_text("hash,submit_unix_us\n" + "".join(f"{h},{u}\n" for h, u in rows))
    return obs, log


REPORT = {"results": dict(attempted=60000, accepted=60000, failed=0, submit_ms=60000)}


class ObserverMetrics(unittest.TestCase):
    def metrics(self, report=REPORT, **kw):
        with tempfile.TemporaryDirectory() as tmp:
            obs, log = synthetic(tmp, **kw)
            return observer.window_metrics(obs, log, report, 1000, 57, 7, 50000)

    def test_window_and_latency_population(self):
        m = self.metrics()
        self.assertAlmostEqual(m["window_s"], 50)
        self.assertEqual(m["submit_to_receipt_s"]["p50"], 2.0)
        self.assertEqual(m["submit_to_finalized_s"]["p99"], 3.0)
        self.assertEqual(m["submit_to_finalized_s"]["n"], 91001)
        self.assertAlmostEqual(m["unfinalized_share"], 1 / 91001)
        self.assertEqual(m["latest_to_finalized_s"]["p50"], 1.0)

    def test_unfinalized_tail_reads_null(self):
        m = self.metrics(unfinalized_from=60)
        self.assertIsNone(m["submit_to_finalized_s"]["p99"])
        self.assertGreater(m["unfinalized_share"], 0.3)

    def test_per_block_attribution(self):
        m = self.metrics()
        pb = m["per_block_median"]
        self.assertEqual(pb["tx_per_block"], 900)
        self.assertEqual(pb["rounds_per_block"], 3)
        self.assertEqual(pb["hold_ms_per_block"], 400)
        self.assertEqual(pb["quorum_wait_ms_per_block"], 200)
        self.assertEqual(m["quorum_wait_ms_exporter"], {"0.99": 600.0})
        self.assertEqual(m["block_interval_ms"]["p50"], 500)
        self.assertAlmostEqual(m["empty_block_share"], 11 / 101)
        self.assertAlmostEqual(m["pool_slope_tx_per_s"], 20.0)
        self.assertGreater(m["cpu_s_per_tx_median"], 0)
        self.assertAlmostEqual(m["cpu_cores_nodes"]["n0"], 1.0)
        self.assertEqual(m["peak_memory_bytes_nodes"]["n3"], 4000 + 11)  # samples 1-11 fall in the window

    def test_cpu_throttling_rounds_and_attribution_per_block(self):
        m = self.metrics()
        # 5 s CPU per 5 s sample, 0.5 s throttled per 5 s, 30 rounds per sample.
        self.assertAlmostEqual(m["throttled_s_per_s_nodes"]["n0"], 0.1)
        self.assertAlmostEqual(m["throttled_period_share_nodes"]["n0"], 0.2)
        self.assertAlmostEqual(m["cpu_s_per_round_nodes"]["n0"], 5 / 30)
        self.assertEqual(m["hold_ms_per_block_by_site_node1"], {"a": 300, "b": 100})
        pb = m["per_block_median"]
        self.assertEqual(pb["duplicate_proposals_per_block"], 2)
        self.assertEqual(pb["deferred_tx_per_block"], 5)
        self.assertEqual(m["fallback_entries_by_cause_nodes"][0], {"carrier_round_below_quorum": 10})
        self.assertEqual(m["largest_block_txs"], 1000)
        self.assertEqual(m["checkpoint_control_rejected_by_reason_nodes"][3], {"lane_full": 20})
        self.assertEqual(m["validator_head_spread_blocks"]["max"], 2)
        self.assertFalse(m["follower_present"])

    def test_verdicts(self):
        self.assertFalse(self.metrics()["generator_bound"])
        slow = {"results": dict(attempted=50000, accepted=50000, failed=0, submit_ms=60000)}
        self.assertTrue(self.metrics(slow)["generator_bound"])
        rejected = {"results": dict(attempted=60000, accepted=59000, failed=1000, submit_ms=60000)}
        self.assertTrue(self.metrics(rejected)["chain_saturated"])
        full = self.metrics(pending=5000)
        self.assertIsNotNone(full["pool_full_at_s"])
        self.assertTrue(full["chain_saturated"])

    def test_log_lines_count_parents_and_work_refusals_by_operation(self):
        counts = dict(unknown_parent_vertex=0, work_refused={}, work_admission_limit_errors=0)
        for line in ["\x1b[33mWARN\x1b[0m x: graph recovery: unknown parent vertex 0xab (absent) of vertex 0xcd round 9",
                     'WARN selected owner Work refused; retrying once Work frees operation="append" attempt=1',
                     'WARN selected owner Work refused; not retrying operation="retire" attempt=16',
                     'WARN selected owner Work refused; retrying once Work frees operation="append" attempt=2',
                     "ERROR cause=certified history Work admission: EncodedWorkBytes requires 1077411095, configured limit is 1073741824",
                     "INFO committed block 12"]:
            observer.count_log_line(line, counts)
        self.assertEqual(counts, dict(unknown_parent_vertex=1, work_admission_limit_errors=1,
                                      work_refused={"append": dict(retried=2, not_retried=0),
                                                    "retire": dict(retried=0, not_retried=1)}))

    def test_prometheus_subset_parse(self):
        text = ('# HELP x\nreth_sync_checkpoint{stage="Finish"} 7\nreth_other 3\n'
                'reth_diesis_mpt_certify_phase_ms{phase="quorum_wait",quantile="0.5"} 9.5\n')
        parsed = observer.parse_prom(text)
        self.assertEqual(parsed[("reth_sync_checkpoint", '{stage="Finish"}')], 7)
        self.assertNotIn(("reth_other", ""), parsed)
        self.assertEqual(observer.quantile(parsed, "0.5"), 9.5)


FILL = observer.TRADE_FILL_TOPIC


def with_methods(tmp):
    """The synthetic run with a method column: every 4th hash a SpotTake, the rest ETHTransfer;
    receipts mark every 10th ETHTransfer reverted, and SpotTakes fill makers 0xa and 0xb."""
    obs, log = synthetic(tmp)
    lines = log.read_text().splitlines()
    out, receipts = [lines[0] + ",method"], {}
    for i, line in enumerate(lines[1:]):
        h = line.split(",")[0]
        method = "SpotTake" if i % 4 == 0 else "ETHTransfer"
        out.append(f"{line},{method}")
        if h in ("0xfirst", "0xdead"):
            continue
        if method == "SpotTake":
            receipts[h] = dict(status=1, gas=137908, makers=["0xa", "0xb"])
        else:
            receipts[h] = dict(status=0 if i % 10 == 1 else 1, gas=21000, makers=[])
    log.write_text("\n".join(out) + "\n")
    return obs, log, receipts


class ObserverOutcomes(unittest.TestCase):
    def metrics(self, drop_receipts=0):
        with tempfile.TemporaryDirectory() as tmp:
            obs, log, receipts = with_methods(tmp)
            for h in list(receipts)[40000:40000 + drop_receipts]:
                receipts.pop(h)
            return observer.window_metrics(obs, log, REPORT, 1000, 57, 7, 50000, receipts=receipts), receipts

    def test_finalized_tps_counts_successful_receipts_only(self):
        m, receipts = self.metrics()
        included = m["finalized_included_tps"] * m["window_s"]
        reverted = m["outcomes"]["finalized_reverted"]
        self.assertEqual(m["finalized_basis"], "successful")
        self.assertGreater(reverted, 0)
        self.assertAlmostEqual(m["finalized_tps"] * m["window_s"], included - reverted)
        self.assertEqual(m["outcomes"]["finalized_status_unknown"], 0)

    def test_per_type_success_latency_gas_and_fills(self):
        m, _ = self.metrics()
        take, transfer = m["per_type"]["SpotTake"], m["per_type"]["ETHTransfer"]
        self.assertEqual(take["finalized_reverted"], 0)
        self.assertGreater(transfer["finalized_reverted"], 0)
        self.assertEqual(take["gas_used_avg"], 137908)
        self.assertEqual(transfer["gas_used_avg"], 21000)
        self.assertEqual(take["fills"], {"2": take["finalized_successful"]})
        self.assertEqual(take["two_distinct_makers"], take["finalized_successful"])
        self.assertEqual(transfer["fills"], {"0": transfer["finalized_successful"]})
        self.assertEqual(take["submit_to_finalized_s"]["p50"], 3.0)
        shares = sum(t["share_of_successful"] for t in m["per_type"].values())
        self.assertAlmostEqual(shares, 1.0)
        # Reverted transfers leave the latency population and show as a share instead.
        self.assertGreater(m["reverted_share"], 0)
        self.assertLess(m["submit_to_finalized_s"]["n"], 91001)

    def test_missing_receipts_are_status_unknown_not_success(self):
        m, _ = self.metrics(drop_receipts=500)
        self.assertGreater(m["outcomes"]["finalized_status_unknown"], 0)
        self.assertAlmostEqual(
            m["finalized_tps"] * m["window_s"],
            m["finalized_included_tps"] * m["window_s"] - m["outcomes"]["finalized_reverted"]
            - m["outcomes"]["finalized_status_unknown"])

    def test_two_column_log_without_receipts_keeps_the_included_basis(self):
        with tempfile.TemporaryDirectory() as tmp:
            obs, log = synthetic(tmp)
            m = observer.window_metrics(obs, log, REPORT, 1000, 57, 7, 50000)
        self.assertEqual(m["finalized_basis"], "included")
        self.assertEqual(set(m["per_type"]), {"unknown"})

    def test_receipt_parse_reads_status_gas_and_distinct_makers(self):
        book = observer.SPOT_BOOK.lower()
        raw = [dict(transactionHash="0x01", status="0x1", gasUsed="0x21aa4", logs=[
                    dict(address=book, topics=[FILL, "0xm", "0xmaker1", "0xt"]),
                    dict(address=book, topics=[FILL, "0xm", "0xmaker2", "0xt"]),
                    dict(address=book, topics=["0xother", "0xm", "0xmaker3", "0xt"])]),
               dict(transactionHash="0x02", status="0x0", gasUsed="0x5208", logs=[])]
        parsed = observer.parse_receipts(raw)
        self.assertEqual(parsed["0x01"], dict(status=1, gas=137892, makers=["0xmaker1", "0xmaker2"]))
        self.assertEqual(parsed["0x02"], dict(status=0, gas=21000, makers=[]))


class Fleet(unittest.TestCase):
    def test_profiles_exist_and_capped_is_the_run_rule(self):
        capped = fleet.read_profile("capped")
        self.assertEqual((capped["FLEET_CPUS"], capped["FLEET_MEMORY"], capped["FLEET_PIN_CPUS"]), ("2", "4g", "true"))
        uncapped = fleet.read_profile("uncapped")
        self.assertNotIn("FLEET_CPUS", uncapped)
        self.assertEqual(uncapped["DIESIS_SELECTED_ENCODED_WORK_BYTES"], "17179869184")

    def test_env_precedence_and_no_ambient_injection(self):
        env = fleet.merged_env({"FLEET_CPUS": "2", "DIESIS_A": "profile", "DIESIS_B": "profile"},
                               {"E2E_MIN_ROUND_DELAY": "20ms", "DIESIS_CARGO_TARGET_DIR": "/scratch",
                                "DIESIS_B": "ambient", "BENCH_DIESIS_ENV": "DIESIS_B=explicit RAYON_NUM_THREADS=4",
                                "PATH": "/bin"})
        self.assertEqual(env, {"DIESIS_A": "profile", "DIESIS_B": "explicit", "DIESIS_MIN_ROUND_DELAY": "20ms",
                               "RAYON_NUM_THREADS": "4"})
        with self.assertRaises(SystemExit):
            fleet.merged_env({}, {"BENCH_DIESIS_ENV": "PATH=/tmp"})

    def test_patch_limits_ports_and_scope(self):
        node = lambda: dict(image="x", build={}, environment={"DIESIS_A": "1"},
                            ports=[dict(target=8545, published="18545")],
                            volumes=[dict(type="volume", source="data")])
        rendered = dict(services={**{n: node() for n in fleet.NODES}, "prometheus": dict(ports=[]),
                                  "node-filter": node(), "test-runner": {}},
                        networks={"n": {}}, volumes={"data": {}, "unused": {}})
        c = fleet.patch(rendered, "sha256:abc", {"DIESIS_A": "2"}, {"FLEET_CPUS": "2", "FLEET_MEMORY": "4g",
                                                                     "FLEET_PIN_CPUS": "true"})
        self.assertEqual(sorted(c["services"]), sorted(fleet.NODES + ["prometheus"]))
        n4 = c["services"]["node-4"]
        self.assertEqual((n4["cpus"], n4["cpuset"], n4["mem_limit"], n4["image"]), (2.0, "6-7", "4g", "sha256:abc"))
        self.assertEqual(n4["environment"]["DIESIS_A"], "2")
        self.assertEqual(n4["ports"][0]["host_ip"], "127.0.0.1")
        self.assertNotIn("build", n4)
        self.assertEqual(c["volumes"], {"data": {"name": "diesis-bench_data"}})
        self.assertEqual(c["networks"], {"n": {"name": "diesis-bench_n"}})
        free = fleet.patch(json.loads(json.dumps(rendered)), "sha256:abc", {}, {})
        self.assertNotIn("cpus", free["services"]["node-1"])
        beside = fleet.patch(json.loads(json.dumps(rendered)), "sha256:abc", {}, {"FLEET_CPUS": "2", "FLEET_PIN_CPUS": "true"},
                             cpu_base=4)
        self.assertEqual([beside["services"][n]["cpuset"] for n in fleet.NODES], ["4-5", "6-7", "8-9", "10-11"])

    def test_cpu_base_keeps_a_lone_fleet_on_0_to_7(self):
        self.assertEqual(fleet.cpuset_cpus("0-1,4"), {0, 1, 4})
        self.assertEqual(fleet.cpuset_cpus(""), set())
        self.assertEqual(fleet.choose_cpu_base(set(), 12, 8), 0)
        self.assertEqual(fleet.choose_cpu_base(set(range(8)), 12, 8), 4)
        self.assertEqual(fleet.choose_cpu_base(set(range(4, 12)), 12, 8), 0)
        self.assertEqual(fleet.choose_cpu_base(set(range(8)), 8, 8), 0)
        self.assertEqual(fleet.choose_cpu_base(set(range(8)), 16, 8), 8)
        lines = "\n".join(["sha256:aa\tcapped\t/capped-node-1\t0-1", "sha256:aa\tcapped\t/capped-node-4\t6-7",
                           "sha256:aa\tuncapped\t/uncapped-node-1\t", "sha256:cc\tother\t/x\t8-9"])
        self.assertEqual(fleet.fleet_containers(lines, {"sha256:aa"}),
                         [("capped", "0-1"), ("capped", "6-7"), ("uncapped", "")])
        with mock.patch.object(fleet, "running_containers", return_value=fleet.fleet_containers(lines, {"sha256:aa"})):
            self.assertEqual(fleet.pinned_cpus(exclude={"diesis-bench"}), {0, 1, 6, 7})
            self.assertEqual(fleet.pinned_cpus(exclude={"capped"}), set())

    def test_interpolation_only_keys_stay_out_of_validator_env(self):
        node = lambda: dict(environment={"RUST_LOG": "info", "DIESIS_MIN_ROUND_DELAY": "50ms"}, ports=[], volumes=[])
        rendered = dict(services={**{n: node() for n in fleet.NODES}, "prometheus": dict(ports=[])}, networks={},
                        volumes={})
        env = {"DIESIS_NODE_RUST_LOG": "info", "DIESIS_MIN_ROUND_DELAY": "20ms",
               "DIESIS_MPT_SERVE_EXECUTED_HEAD": "true"}
        c = fleet.patch(rendered, "sha256:abc", env, {}, {"DIESIS_NODE_RUST_LOG", "DIESIS_MIN_ROUND_DELAY"})
        e = c["services"]["node-1"]["environment"]
        self.assertNotIn("DIESIS_NODE_RUST_LOG", e)
        self.assertEqual(e["DIESIS_MIN_ROUND_DELAY"], "20ms")
        self.assertEqual(e["DIESIS_MPT_SERVE_EXECUTED_HEAD"], "true")

    def test_port_blocks_never_overlap(self):
        blocks = [set(fleet.host_ports(fleet.block_ports(k))) for k in range(fleet.MAX_BLOCKS)]
        for i in range(len(blocks)):
            for j in range(i + 1, len(blocks)):
                self.assertFalse(blocks[i] & blocks[j], (i, j))
        subnets = {fleet.block_ports(k)["subnet_prefix"] for k in range(fleet.MAX_BLOCKS)}
        self.assertEqual(len(subnets), fleet.MAX_BLOCKS)
        self.assertNotIn("10.100.0", subnets)  # the make e2e-up default

    def test_choose_block_keeps_previous_and_skips_busy(self):
        self.assertEqual(fleet.choose_block(None, set(), set())["block"], 0)
        self.assertEqual(fleet.choose_block(3, set(), set())["block"], 3)
        busy0 = {fleet.block_ports(0)["rpc"][2]}
        self.assertEqual(fleet.choose_block(None, busy0, set())["block"], 1)
        self.assertEqual(fleet.choose_block(0, busy0, set())["block"], 1)
        self.assertEqual(fleet.choose_block(None, set(), {"10.100.100", "10.100.101"})["block"], 2)
        everything = {p for k in range(fleet.MAX_BLOCKS) for p in fleet.host_ports(fleet.block_ports(k))}
        with self.assertRaises(SystemExit):
            fleet.choose_block(None, everything, set())

    def test_containers_are_named_per_project(self):
        node = lambda: dict(environment={}, ports=[], volumes=[])
        rendered = dict(services={**{n: node() for n in fleet.NODES}, "prometheus": dict(ports=[])}, networks={},
                        volumes={})
        c = fleet.patch(rendered, "sha256:abc", {}, {}, project="alice-bench")
        self.assertEqual(c["services"]["node-3"]["container_name"], "alice-bench-node-3")
        self.assertEqual(c["services"]["prometheus"]["container_name"], "alice-bench-prometheus")

    def test_shell_env_routes_through_the_selected_block(self):
        import contextlib, io, os, shlex
        with tempfile.TemporaryDirectory() as tmp:
            old = dict(os.environ)
            try:
                os.environ.update(BENCH_RESULTS_BASE=tmp, BENCH_DIESIS_FLEET="alice-bench")
                os.environ.pop("DIESIS_AGED_TAG", None)
                path = fleet.ports_path("diesis-bench-aged-old")
                path.parent.mkdir(parents=True)
                path.write_text(json.dumps(dict(fleet.block_ports(4), containers=["a", "b", "c", "d"],
                                                node_ips=["10.100.104.11", "x", "y", "z"])))
                out = io.StringIO()
                with contextlib.redirect_stdout(out):
                    fleet.cmd_shell_env(None)
                fresh = dict(line.split("=", 1) for line in out.getvalue().splitlines())
                self.assertEqual(fresh["FLEET_BLOCK"], "0")
                self.assertIn("alice-bench-node-1", shlex.split(fresh["FLEET_CONTAINERS"])[0])
                os.environ["DIESIS_AGED_TAG"] = "old"
                out = io.StringIO()
                with contextlib.redirect_stdout(out):
                    fleet.cmd_shell_env(None)
                aged = {k: shlex.split(v)[0] for k, v in (l.split("=", 1) for l in out.getvalue().splitlines())}
                self.assertEqual(aged["FLEET_BLOCK"], "4")
                self.assertTrue(aged["FLEET_RPC"].startswith("http://127.0.0.1:19345"))
                self.assertIn("NODE1_CONTAINER=a NODE1_IP=10.100.104.11", aged["FLEET_TOPOLOGY"])
            finally:
                os.environ.clear()
                os.environ.update(old)

    def test_fleet_slots_count_projects_by_image_id(self):
        lines = "\n".join(["sha256:aa\towner-lock-cuts\t/owner-lock-cuts-node-1",
                           "sha256:aa\towner-lock-cuts\t/owner-lock-cuts-node-2",
                           "sha256:bb\tdiesis-bench\t/diesis-bench-node-1",
                           "sha256:cc\tcoordin\t/coordin-api-1",
                           "sha256:aa\t\t/loose-node",
                           "sha256:aa\t<no value>\t/other-node"])
        self.assertEqual(fleet.fleet_projects(lines, {"sha256:aa", "sha256:bb"}),
                         {"owner-lock-cuts", "diesis-bench", "loose-node", "other-node"})

    def test_wait_for_slot_waits_while_the_host_is_full(self):
        with mock.patch.dict(os.environ, {"BENCH_DIESIS_MAX_FLEETS": ""}), \
                mock.patch.object(fleet, "running_fleets") as running:
            fleet.wait_for_slot("diesis-bench")
            running.assert_not_called()
        with mock.patch.dict(os.environ, {"BENCH_DIESIS_MAX_FLEETS": "2"}), \
                mock.patch.object(fleet, "running_fleets", side_effect=[{"a", "b"}, {"a", "b"}, {"a"}]) as running, \
                mock.patch.object(fleet.time, "sleep") as sleep, mock.patch.object(fleet, "say"):
            fleet.wait_for_slot("diesis-bench")
        self.assertEqual(running.call_count, 3)
        self.assertEqual(sleep.call_count, 2)
        running.assert_called_with(exclude={"diesis-bench"})

    def test_hold_file_yields_the_slot_until_removed(self):
        with tempfile.TemporaryDirectory() as tmp:
            hold = Path(tmp) / "HOLD"
            hold.touch()
            sleeps = []

            def sleep(_seconds):
                sleeps.append(1)
                if len(sleeps) == 2:
                    hold.unlink()

            with mock.patch.dict(os.environ, {"BENCH_DIESIS_MAX_FLEETS": "", "BENCH_DIESIS_HOLD_FILE": str(hold)}), \
                    mock.patch.object(fleet, "running_fleets") as running, \
                    mock.patch.object(fleet.time, "sleep", side_effect=sleep), mock.patch.object(fleet, "say"):
                fleet.wait_for_slot("diesis-bench")
            running.assert_not_called()
            self.assertEqual(len(sleeps), 2)

    def test_fixture_name_carries_image_and_genesis(self):
        m = dict(tag="m6570-5k", genesis_hash="0x" + "ab" * 32, provenance=dict(image_id="sha256:" + "7d" * 32))
        self.assertEqual(fleet.fixture_name(m), "m6570-5k-7d7d7d7d7d7d-abababab" + "ab")

    def test_aged_tags_are_restricted(self):
        self.assertEqual(fleet.aged_project("m832-5k"), "diesis-bench-aged-m832-5k")
        with self.assertRaises(SystemExit):
            fleet.aged_project("../x")

    def test_outcome_rule_defaults_to_bls_only_with_its_epoch_zero_artifact(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            # A checkout before the genesis control gets no rule keys its node would refuse.
            self.assertEqual(fleet.outcome_rule_env(repo, {}), ({}, None))
            with self.assertRaises(SystemExit):
                fleet.outcome_rule_env(repo, {"BENCH_DIESIS_OUTCOME_RULE": "bls-only"})
            (repo / "docker" / "epoch").mkdir(parents=True)
            (repo / "docker" / "epoch" / fleet.BLS_ONLY_EPOCH_ZERO).write_text("0x00\n")
            bls_only = {"DIESIS_DEV_OUTCOME_SIGNATURE_RULE": "bls-only",
                        "DIESIS_EPOCH_ZERO_CALLDATA_FILE": "/etc/diesis/epoch/epoch-zero-4v-bls-only.calldata"}
            self.assertEqual(fleet.outcome_rule_env(repo, {}), (bls_only, "bls-only"))
            self.assertEqual(fleet.outcome_rule_env(repo, {"BENCH_DIESIS_OUTCOME_RULE": "hybrid"}),
                             ({"DIESIS_DEV_OUTCOME_SIGNATURE_RULE": "hybrid"}, "hybrid"))
            # An aged fleet's recorded rule is the default, and an explicit request still wins.
            self.assertEqual(fleet.outcome_rule_env(repo, {}, "hybrid")[1], "hybrid")
            self.assertEqual(fleet.outcome_rule_env(repo, {"BENCH_DIESIS_OUTCOME_RULE": "bls-only"}, "hybrid"),
                             (bls_only, "bls-only"))
            for unknown in ("BLS-only", "bls_only", "true"):
                with self.assertRaises(SystemExit):
                    fleet.outcome_rule_env(repo, {"BENCH_DIESIS_OUTCOME_RULE": unknown})

    def test_outcome_rule_artifact_replaces_the_compose_default(self):
        node = lambda: dict(environment={"DIESIS_EPOCH_ZERO_CALLDATA_FILE": "/etc/diesis/epoch/epoch-zero-4v.calldata"},
                            ports=[], volumes=[])
        rendered = dict(services={**{n: node() for n in fleet.NODES}, "prometheus": dict(ports=[])}, networks={},
                        volumes={})
        env = {"DIESIS_DEV_OUTCOME_SIGNATURE_RULE": "bls-only",
               "DIESIS_EPOCH_ZERO_CALLDATA_FILE": "/etc/diesis/epoch/epoch-zero-4v-bls-only.calldata"}
        c = fleet.patch(rendered, "sha256:abc", env, {}, {"DIESIS_E2E_CHAIN"})
        for name in fleet.NODES:
            self.assertEqual({k: c["services"][name]["environment"][k] for k in env}, env)


class AB(unittest.TestCase):
    def test_schedule_interleaves(self):
        self.assertEqual(ab.schedule(3, True), list("abbaab"))
        self.assertEqual(ab.schedule(3, False), list("aaa"))

    def test_side_parsing(self):
        side = ab.parse_side("profile=uncapped,image=t,env=DIESIS_A=1+DIESIS_B=2,name=cut")
        self.assertEqual((side["profile"], side["env"]), ("uncapped", ["DIESIS_A=1", "DIESIS_B=2"]))
        self.assertEqual(ab.parse_side("")["profile"], "capped")
        with self.assertRaises(SystemExit):
            ab.parse_side("profile=fast")
        with self.assertRaises(SystemExit):
            ab.parse_side("env=PATH=/x")
        self.assertEqual(ab.parse_side("outcome=hybrid,name=pq")["outcome"], "hybrid")
        self.assertNotIn("outcome", ab.parse_side("profile=uncapped"))
        self.assertEqual(ab.parse_side("test=evm")["test"], "evm")
        with self.assertRaises(SystemExit):
            ab.parse_side("test=orderbook")
        with self.assertRaises(SystemExit):
            ab.parse_side("outcome=ml-dsa")

    def test_probe_step_and_overload(self):
        plan = dict(rates=[1000], duration=300, warmup=0, probe="2:30", max_load=40)
        self.assertEqual(ab.steps_of(plan), [("probe", 2, 30, 0), ("1000", 1000, 300, 0)])
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp)
            for step, load in (("probe", 12.0), ("1000", 55.0)):
                d = out / "arm-1a" / step
                d.mkdir(parents=True)
                (d / "diesis-metrics.json").write_text(json.dumps(dict(host_load_1m=dict(median=load, max=load))))
            self.assertEqual(ab.overloaded(out, 1, "a", plan), 55.0)
            self.assertIsNone(ab.overloaded(out, 1, "a", dict(plan, max_load=60)))

    def test_summary_medians_and_ranges(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp)
            with tempfile.TemporaryDirectory() as tmp2:
                obs, log = synthetic(tmp2)
                base = observer.window_metrics(obs, log, REPORT, 1000, 57, 7, 50000)
            plan = dict(sides={"a": ab.parse_side("profile=capped"), "b": ab.parse_side("profile=uncapped")},
                        order=list("abba"), rates=[1000], note="Both sides: DIESIS_X=2")
            for (i, key), factor in zip(enumerate(plan["order"], 1), (1.0, 1.1, 0.9, 1.2)):
                d = out / f"arm-{i}{key}" / "1000"
                d.mkdir(parents=True)
                (d / "diesis-metrics.json").write_text(json.dumps(dict(base, finalized_tps=1000 * factor)))
            ab.summarize(out, plan)
            summary = json.loads((out / "summary.json").read_text())
            b = summary["b"]["rates"]["1000"]
            self.assertEqual(b["n"], 2)
            self.assertAlmostEqual(b["finalized_tps"]["median"], 1000)
            self.assertAlmostEqual(b["finalized_tps"]["min"], 900)
            md = (out / "summary.md").read_text()
            self.assertIn("| 1000 |", md)
            self.assertIn("> Both sides: DIESIS_X=2", md)
            self.assertIn("host_load_median", md)


class Judged(unittest.TestCase):
    def test_refusal_records_work_admission_and_exit_reasons(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            (d / "node-3.log").write_text(
                "x INFO start\n\x1b[31mERROR\x1b[0m reth::cli: shutting down due to error err=start selected certified "
                "runtime: certified history Work admission: EncodedWorkBytes requires 1084835969, configured limit is "
                "1073741824\nError: start selected certified runtime: certified history Work admission: EncodedWorkBytes "
                "requires 1084835969, configured limit is 1073741824\n")
            (d / "node-3.state.json").write_text(json.dumps(dict(ExitCode=1, OOMKilled=False)))
            (d / "node-1.log").write_text("y ERROR reth::cli: shutting down due to error err=MPT proposal quorum deadline\n")
            (d / "node-1.state.json").write_text("")
            r = judged.refusal(d)
        self.assertEqual(r["work_admission"], dict(node="node-3", admission="certified history Work admission",
                                                   kind="EncodedWorkBytes", requested_bytes=1084835969,
                                                   limit_bytes=1073741824))
        self.assertEqual(r["nodes"]["node-3"]["exit_code"], 1)
        self.assertIn("quorum deadline", r["nodes"]["node-1"]["exit_reason"])

    def test_summary_marks_busy_windows_functional(self):
        with tempfile.TemporaryDirectory() as tmp:
            obs, log = synthetic(tmp)
            m = observer.window_metrics(obs, log, REPORT, 1000, 57, 7, 50000)
        m["host_load_1m"] = dict(median=10, max=25)
        s = judged.summary(m, 18)
        self.assertTrue(s["functional_only"])
        self.assertEqual(s["submit_to_finalized_s"]["p50"], 3.0)
        self.assertIn("n0", s["peak_memory_bytes_nodes"])
        self.assertFalse(judged.summary(dict(m, host_load_1m=dict(median=5, max=12)), 18)["functional_only"])

    def test_judged_window_shared_with_another_fleet_is_functional_only(self):
        quiet = dict(functional_only=False)
        self.assertEqual(judged.mark_shared(quiet, [], []), dict(functional_only=False, shared_host=False))
        self.assertEqual(judged.mark_shared(quiet, [], ["p3-unblock"]), dict(functional_only=True, shared_host=True))
        self.assertTrue(judged.mark_shared(dict(functional_only=True), [], [])["functional_only"])
        md = judged.markdown(dict(tag="t", fixture=None, profile="capped", rate=1000, status="refused", image="i",
                                  fixture_height=5100, cpusets={"node-1": "0-1"}, other_fleets_at_start=[],
                                  other_fleets_at_end=["x"], refusal=dict(nodes={}, work_admission=None)))
        self.assertIn("- Validator cpusets: node-1 0-1", md)
        self.assertIn("start / end: none / ['x']", md)


def idle_arm(number, stretch, cpu, blocks_per_s):
    return dict(arm=number, stretch=stretch, result="completed",
                idle=dict(cpu_pct={f"n{i}": cpu + i for i in range(4)}, cpu_pct_median=cpu + 1.5,
                          host_load_1m=dict(median=3.0, max=4.0)),
                idle_heads=dict(all_advanced=True, blocks_per_s=[blocks_per_s] * 4),
                idle_interval_ms=dict(p50=1000 / blocks_per_s, p90=None, p99=1200 / blocks_per_s, n=10),
                probes=[dict(first_block_s=0.5, receipt_s=0.9, finalized_s=1.5)],
                first_block_s=dict(median=0.5, min=0.5, max=0.5, n=1), receipt_s=dict(median=0.9, min=0.9, max=0.9, n=1),
                finalized_s=dict(median=1.5, min=1.5, max=1.5, n=1), agreement=dict(agree=True, height=5),
                not_running=[], idle_rounds_per_s=[2 * blocks_per_s] * 4,
                idle_mode={f"n{i}": 1 for i in range(4)})


class Idle(unittest.TestCase):
    def test_arms_interleave(self):
        self.assertEqual(idle.schedule(["4", "1"], 2), ["4", "1", "1", "4"])
        self.assertEqual(idle.schedule(["4", "1"], 1), ["4", "1"])
        self.assertEqual(idle.schedule(["4", "1"], 3), ["4", "1", "1", "4", "4", "1"])

    def test_gaps_spread_multi_block_polls(self):
        for got, want in zip(idle.gaps_ms([(0.0, 1), (0.8, 1), (2.4, 2)]), [800.0, 800.0], strict=True):
            self.assertAlmostEqual(got, want)
        self.assertEqual(idle.gaps_ms([(0.0, 1)]), [])

    def test_rounds_per_second_skip_failed_scrapes(self):
        self.assertEqual(idle.per_second([10, None, 4], [30, 9, 8], 10.0), [2.0, None, 0.4])

    def test_advance_requires_every_head_to_move(self):
        start = dict(at=0.0, latest=[10, 10, 10, 10], finalized=[8, 8, 8, 8])
        end = dict(at=10.0, latest=[20, 21, 20, 20], finalized=[18, 18, 18, 18])
        moved = idle.advance(start, end)
        self.assertTrue(moved["all_advanced"])
        self.assertAlmostEqual(moved["blocks_per_s"][1], 1.1)
        self.assertFalse(idle.advance(start, dict(end, finalized=[18, 8, 18, 18]))["all_advanced"])
        self.assertFalse(idle.advance(start, dict(end, latest=[20, None, 20, 20]))["all_advanced"])

    def test_report_has_a_row_per_arm_and_medians_per_stretch(self):
        arms = [idle_arm(1, 4, 10.0, 1.0), idle_arm(2, 1, 30.0, 4.0), idle_arm(3, 1, 32.0, 4.0),
                idle_arm(4, 4, 12.0, 1.0), dict(arm=5, stretch=4, result="failed: x")]
        windows = [dict(seconds=90.0, cpu_pct={"n0": c, "n1": c + 2}, host_load_1m=dict(median=5.0, max=6.0),
                        heads=dict(blocks_per_s=[b, b], all_advanced=True)) for c, b in ((10.0, 0.5), (14.0, 0.7))]
        arms[0]["gap_idle"] = idle.gap_summary(windows)
        self.assertEqual(arms[0]["gap_idle"]["cpu_pct"], {"n0": 12.0, "n1": 14.0})
        self.assertIsNone(idle.gap_summary([]))
        md = idle.report(dict(profile="capped", idle_s=300, probes=1, probe_idle_s=120, arms=arms))
        self.assertIn("| 1 | 4 | 12.0, 14.0 | 13.0 | 0.60 | yes | 5.0/6.0 |", md)
        self.assertIn("| 2 | 1 | - | - | - | - | - |", md)
        self.assertIn("| 1 | 4 | 10.0, 11.0, 12.0, 13.0 | 11.5 | 1.00 | 2.00 | 1000/1200 | 0.50 | 0.90 | "
                      "1.50 (1.50–1.50, n=1) | yes; agree True | 1/1, 1/1, 1/1, 1/1 | 3.0/4.0 |", md)
        self.assertIn("| 5 | 4 | failed: x |", md)
        self.assertIn("| 4 | 2 | 12.5 (10.0–15.0) | 1.00 | 2.00 | 1000/1200 | 0.50 | 0.90 | 1.50 (1.50–1.50, n=2) | "
                      "2/2 |", md)
        self.assertIn("| 1 | 2 | 32.5 (30.0–35.0) | 4.00 | 8.00 | 250/300 |", md)


if __name__ == "__main__":
    unittest.main()


def timeline_line(kind, us, **fields):
    target, message = (("diesis_round_timeline", "round timeline") if kind == "round"
                       else ("diesis_mpt_vote_timeline", "mpt vote timeline"))
    body = " ".join(f'{k}="{v}"' if isinstance(v, str) else f"{k}={v}" for k, v in fields.items())
    return f"2026-11-22T00:00:00.000000Z  INFO {target}: {message} {body} unix_us={int(us * 1e6)}\n"


class TimelineAnalysis(unittest.TestCase):
    """Two sampled transfers submitted between rounds 10 and 11: A to node 1, which owns its
    sender and carries it at round 11; B to node 2, which does not, so node 3 carries it at 12.
    Both land in block 5, cut from the Commit of leader round 14."""

    def test_owner_split_and_stages(self):
        A, B = "0x" + "a" * 64, "0x" + "b" * 64
        t = lambda r, node: 100 + r * 0.2 + node * 0.001
        lines = {n: [] for n in range(1, 5)}
        for n in range(1, 5):
            for r in range(10, 20):
                sampled = A if (n, r) == (1, 11) else B if (n, r) == (3, 12) else ""
                lines[n].append(timeline_line("round", t(r, n), event="vertex_signed", round=r,
                                              leader="true" if (r == 14 and n == 2) else "false",
                                              mode="partitioned", slot=n - 1, proposers=4, carried=1 if sampled else 0,
                                              sampled=sampled))
        lines[1].append(timeline_line("round", 102.05, event="tx_admitted", tx=A, sender="0x1", origin="local"))
        lines[2].append(timeline_line("round", 102.05, event="tx_admitted", tx=B, sender="0x2", origin="local"))
        for n in (1, 3, 4):
            lines[n].append(timeline_line("round", 102.1, event="tx_admitted", tx=B, sender="0x2", origin="external"))
        lines[1].append(timeline_line("round", t(11, 1) - 0.0005, event="tx_selection", round=11, tx=A,
                                      verdict="primary", owner=0, local=0, proposers=4))
        lines[2].append(timeline_line("round", t(11, 2) - 0.0005, event="tx_selection", round=11, tx=B,
                                      verdict="owner_live", owner=2, local=1, proposers=4))
        lines[3].append(timeline_line("round", t(12, 3) - 0.0005, event="tx_selection", round=12, tx=B,
                                      verdict="primary", owner=2, local=2, proposers=4))
        lines[1].append(timeline_line("vote", 103.39, event="round", number=17, detail=""))
        lines[1].append(timeline_line("round", 103.6, event="decided", round=14, offset=0, value="commit",
                                      path="carried_quorum", view=0))
        lines[1].append(timeline_line("round", 103.7, event="installed", round=14, offset=0, changed="true"))
        lines[1].append(timeline_line("round", 103.9, event="block_cut", number=5, round=14, included=2, deferred=0))
        lines[2].append(timeline_line("vote", 104.0, event="requested", number=5, detail=""))
        lines[2].append(timeline_line("vote", 104.2, event="quorum", number=5, detail="collector"))
        with tempfile.TemporaryDirectory() as tmp:
            for n, body in lines.items():
                Path(tmp, f"timeline-bench-node-{n}.log").write_text("".join(body))
            Path(tmp, "timeline-blocks.json").write_text(json.dumps(dict(
                sampled={A: dict(number=5, seen=104.3, finalized=104.4), B: dict(number=5, seen=104.3, finalized=104.4)},
                blocks={})))
            s = timeline.analyze(tmp)
            rows = {r["tx"]: r for r in json.loads(Path(tmp, "stages.json").read_text())["txs"]}
        self.assertEqual(s["sampled_txs"], 2)
        owner, other = s["inclusion"]["owner"], s["inclusion"]["non_owner"]
        self.assertEqual((owner["n"], owner["first_opportunity_share"]), (1, 1.0))
        self.assertEqual((other["n"], other["first_opportunity_share"], other["extra_rounds"]["p50"]), (1, 0.0, 1))
        self.assertAlmostEqual(rows[A]["a"], t(11, 1) - 102.05, places=4)
        self.assertAlmostEqual(rows[A]["b"], t(14, 2) - t(11, 1), places=4)
        self.assertAlmostEqual(rows[A]["c"], 103.39 - t(14, 2), places=4)
        self.assertAlmostEqual(rows[A]["d1"], 103.6 - 103.39, places=4)
        self.assertAlmostEqual(rows[B]["g"], 104.0 - 103.9, places=4)
        self.assertAlmostEqual(rows[B]["h"], 0.2, places=4)
        self.assertAlmostEqual(rows[B]["total"], 104.4 - 102.05, places=4)
        self.assertEqual(rows[B]["rounds_carry_to_leader"], 2)
        self.assertAlmostEqual(s["round_period_s"]["p50"], 0.2)
