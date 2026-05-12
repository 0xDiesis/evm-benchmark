#!/usr/bin/env python3
"""Unit tests for prometheus-diagnostics.py helpers."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path
from urllib.parse import parse_qs, urlsplit


SCRIPT = Path(__file__).with_name("prometheus-diagnostics.py")
SPEC = importlib.util.spec_from_file_location("prometheus_diagnostics", SCRIPT)
assert SPEC is not None
prometheus_diagnostics = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(prometheus_diagnostics)


class PrometheusDiagnosticsTests(unittest.TestCase):
    def assert_query_url(self, base_url: str, expected_path: str) -> None:
        url = prometheus_diagnostics.prometheus_query_url(base_url, "sum(metric_total)")
        parsed = urlsplit(url)
        self.assertEqual(parsed.path, expected_path)
        self.assertEqual(parse_qs(parsed.query), {"query": ["sum(metric_total)"]})

    def test_prometheus_query_url_appends_api_path_to_root(self) -> None:
        self.assert_query_url("http://localhost:9090", "/api/v1/query")

    def test_prometheus_query_url_preserves_prefixed_prometheus_path(self) -> None:
        self.assert_query_url("http://localhost:9090/prometheus", "/prometheus/api/v1/query")

    def test_prometheus_query_url_does_not_duplicate_api_query_path(self) -> None:
        self.assert_query_url("http://localhost:9090/api/v1/query", "/api/v1/query")

    def test_prometheus_query_url_strips_unrelated_existing_query(self) -> None:
        url = prometheus_diagnostics.prometheus_query_url(
            "http://localhost:9090/api/v1/query?old=value",
            "up",
        )
        parsed = urlsplit(url)
        self.assertEqual(parsed.path, "/api/v1/query")
        self.assertEqual(parse_qs(parsed.query), {"query": ["up"]})


if __name__ == "__main__":
    unittest.main()
