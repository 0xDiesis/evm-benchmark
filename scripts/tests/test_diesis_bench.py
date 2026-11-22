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
            ("reth_transaction_pool_pending_pool_transactions", ""): pending * k}


def synthetic(tmp, pending=100, unfinalized_from=None):
    """120 blocks 0.5 s apart from W0, 1,000 txs each (every 10th empty), finalized 1 s after first
    seen; each tx submitted 2 s before its block was seen; one hash never included. Load starts
    at W0 - 2 (first submission), so warmup 7 s and duration 57 s give the window [W0+5, W0+55]."""
    obs = types.SimpleNamespace(lock=threading.Lock(), blocks={}, samples=[], exited=[], errors=[])
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
                            {f"n{i}": (i + 1) * 1000 + k for i in range(4)}))
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

    def test_verdicts(self):
        self.assertFalse(self.metrics()["generator_bound"])
        slow = {"results": dict(attempted=50000, accepted=50000, failed=0, submit_ms=60000)}
        self.assertTrue(self.metrics(slow)["generator_bound"])
        rejected = {"results": dict(attempted=60000, accepted=59000, failed=1000, submit_ms=60000)}
        self.assertTrue(self.metrics(rejected)["chain_saturated"])
        full = self.metrics(pending=5000)
        self.assertIsNotNone(full["pool_full_at_s"])
        self.assertTrue(full["chain_saturated"])

    def test_prometheus_subset_parse(self):
        text = ('# HELP x\nreth_sync_checkpoint{stage="Finish"} 7\nreth_other 3\n'
                'reth_diesis_mpt_certify_phase_ms{phase="quorum_wait",quantile="0.5"} 9.5\n')
        parsed = observer.parse_prom(text)
        self.assertEqual(parsed[("reth_sync_checkpoint", '{stage="Finish"}')], 7)
        self.assertNotIn(("reth_other", ""), parsed)
        self.assertEqual(observer.quantile(parsed, "0.5"), 9.5)


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

    def test_fixture_name_carries_image_and_genesis(self):
        m = dict(tag="m6570-5k", genesis_hash="0x" + "ab" * 32, provenance=dict(image_id="sha256:" + "7d" * 32))
        self.assertEqual(fleet.fixture_name(m), "m6570-5k-7d7d7d7d7d7d-abababab" + "ab")

    def test_aged_tags_are_restricted(self):
        self.assertEqual(fleet.aged_project("m832-5k"), "diesis-bench-aged-m832-5k")
        with self.assertRaises(SystemExit):
            fleet.aged_project("../x")


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
                        order=list("abba"), rates=[1000])
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
            self.assertIn("| 1000 |", (out / "summary.md").read_text())


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
        md = idle.report(dict(profile="capped", idle_s=300, probes=1, probe_idle_s=120, arms=arms))
        self.assertIn("| 1 | 4 | 10.0, 11.0, 12.0, 13.0 | 11.5 | 1.00 | 2.00 | 1000/1200 | 0.50 | 0.90 | "
                      "1.50 (1.50–1.50, n=1) | yes; agree True | 1/1, 1/1, 1/1, 1/1 | 3.0/4.0 |", md)
        self.assertIn("| 5 | 4 | failed: x |", md)
        self.assertIn("| 4 | 2 | 12.5 (10.0–15.0) | 1.00 | 2.00 | 1000/1200 | 0.50 | 0.90 | 1.50 (1.50–1.50, n=2) | "
                      "2/2 |", md)
        self.assertIn("| 1 | 2 | 32.5 (30.0–35.0) | 4.00 | 8.00 | 250/300 |", md)


if __name__ == "__main__":
    unittest.main()
