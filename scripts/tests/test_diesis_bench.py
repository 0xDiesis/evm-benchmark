#!/usr/bin/env python3
"""Offline tests for the Diesis fleet, observer and A/B runner (no Docker, no chain)."""
import importlib.util
import json
from pathlib import Path
import tempfile
import threading
import types
import unittest

ROOT = Path(__file__).resolve().parents[2]


def load(path):
    spec = importlib.util.spec_from_file_location(path.stem.replace("-", "_"), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


observer = load(ROOT / "bench-targets/scripts/diesis-observer.py")
fleet = load(ROOT / "bench-targets/chains/diesis/fleet.py")
ab = load(ROOT / "bench-targets/scripts/ab.py")

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
        obs.samples.append((W0 + k * 5, [prom(k, pending)] * 4, {f"n{i}": int(k * 5e9) for i in range(4)}, 4.0))
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

    def test_env_precedence(self):
        env = fleet.merged_env({"FLEET_CPUS": "2", "DIESIS_A": "profile", "DIESIS_B": "profile"},
                               {"E2E_MIN_ROUND_DELAY": "20ms", "DIESIS_B": "env", "DIESIS_REPO_DIR": "/x",
                                "BENCH_DIESIS_ENV": "DIESIS_C=extra RAYON_NUM_THREADS=4", "PATH": "/bin"})
        self.assertEqual(env, {"DIESIS_A": "profile", "DIESIS_B": "env", "DIESIS_MIN_ROUND_DELAY": "20ms",
                               "DIESIS_C": "extra", "RAYON_NUM_THREADS": "4"})
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
        self.assertEqual(list(c["volumes"]), ["data"])
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


if __name__ == "__main__":
    unittest.main()
