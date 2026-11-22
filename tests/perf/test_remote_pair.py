import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch

from remote_pair import STAGE_CONTROLLER, RelayMonitor, candidate_binaries, main, quic_observation_coverage, release_binaries, transport_comparison_can_continue
from native_support import Artifacts


class ReleaseTests(unittest.TestCase):
    def test_candidate_binary_is_explicit_modified_and_checksum_verified(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "input"
            binary.write_bytes(b"candidate-only")
            (root / "candidate-source.tar.gz").write_bytes(b"retained-source")
            (root / "instrumentation.patch").write_bytes(b"retained-diff")
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps(dict(base_commit="8342a7141bf27fb5b30adca224d759fa2a7aa0bc",
                target="x86_64-unknown-linux-gnu", feature="perf-quic-stats",
                binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                source_archive_sha256=hashlib.sha256(b"retained-source").hexdigest(),
                instrumentation_sha256=hashlib.sha256(b"retained-diff").hexdigest())))
            binaries = candidate_binaries(root, binary, manifest)
            self.assertEqual(binaries["controller"]["provenance"], "modified")
            self.assertEqual(binaries["controller"]["sha256"], binaries["target"]["sha256"])
            binary.write_bytes(b"changed")
            with self.assertRaisesRegex(AssertionError, "checksum mismatch"):
                candidate_binaries(root, binary, manifest)

    def test_candidate_observations_require_both_roles_and_stable_connection(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "logs").mkdir()
            (root / "target/logs").mkdir(parents=True)
            (root / "metadata.json").write_text(json.dumps(dict(diagnostic_cases=[dict(case="initial_high",
                flows=[dict(large_started_controller_utc=10, large_finished_controller_utc=12)])])))
            for role, name in (("attachment", "logs/attachment.log"), ("host", "target/logs/host.log")):
                (root / name).write_text("\n".join("LOCHO_QUIC_STATS " + json.dumps(dict(schema=1, role=role,
                    connection_stable_id=123, elapsed_ms=i * 1000, utc_ms=10000 + i * 1000, paths=[{}])) for i in range(3)))
            self.assertEqual(quic_observation_coverage(root, "diagnostic")["result"], "passed")
            (root / "target/logs/host.log").write_text("official binary without instrumentation")
            self.assertEqual(quic_observation_coverage(root, "diagnostic")["result"], "failed")

    def test_candidate_soak_allows_only_scheduled_process_epochs_and_requires_timeout_samples(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "logs").mkdir()
            (root / "target/logs").mkdir(parents=True)
            (root / "metadata.json").write_text(json.dumps(dict(controller_exit={"generation": 2})))
            (root / "operations.jsonl").write_text(json.dumps(dict(outcome="failed", expected=False, utc=16,
                combination="tt", telemetry={"read_phase": "response_payload"})) + "\n")
            for role, name in (("attachment", "logs/attachment.log"), ("host", "target/logs/host.log")):
                rows = []
                for i in range(9):
                    # The restarted attachment deliberately reuses a numerical
                    # process-local ID: elapsed reset must still split epochs.
                    rows.append("LOCHO_QUIC_STATS " + json.dumps(dict(schema=1, role=role,
                        connection_stable_id=123, elapsed_ms=(i % 5) * 1000,
                        utc_ms=10000 + i * 1000, paths=[{}])))
                (root / name).write_text("\n".join(rows))
            coverage = quic_observation_coverage(root, "soak")
            self.assertEqual(coverage["result"], "passed")
            self.assertEqual(coverage["processes"]["attachment"]["observed_connection_epochs"], 2)
            self.assertEqual(coverage["processes"]["host"]["observed_connection_epochs"], 2)
            self.assertEqual(coverage["processes"]["host"]["expected_connection_epochs"], 2)
            (root / "operations.jsonl").write_text(json.dumps(dict(outcome="failed", expected=False, utc=30,
                telemetry={"read_phase": "response_payload"})) + "\n")
            self.assertEqual(quic_observation_coverage(root, "soak")["result"], "failed")

    def test_failed_private_staging_removes_only_fresh_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            unrelated = Path(directory) / "keep"
            unrelated.write_text("user work")
            settings = dict(parent=directory, files={}, binary_size=3, binary_sha256="0" * 64, run_id="test-owner")
            result = subprocess.run([sys.executable, "-B", "-c", STAGE_CONTROLLER],
                input=json.dumps(settings).encode() + b"\nabc", capture_output=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(list(Path(directory).iterdir()), [unrelated])
            self.assertEqual(unrelated.read_text(), "user work")

    def release(self):
        archives = {}
        assets = []
        for triple in ("x86_64-unknown-linux-gnu", "aarch64-apple-darwin"):
            binary = triple.encode()
            stream = io.BytesIO()
            with tarfile.open(fileobj=stream, mode="w:xz") as archive:
                member = tarfile.TarInfo("locho")
                member.size = len(binary)
                archive.addfile(member, io.BytesIO(binary))
            data = stream.getvalue()
            url = "https://example.org/" + triple
            archives[url] = data
            assets.append(dict(name=f"locho-{triple}.tar.xz", browser_download_url=url,
                               digest="sha256:" + hashlib.sha256(data).hexdigest()))
        release = dict(prerelease=False, tag_name="v1.3.0", target_commitish="reviewed-commit", assets=assets)

        def fetch(url, timeout):
            return io.BytesIO(archives[url] if url in archives else json.dumps(release).encode())

        return release, archives, fetch

    def test_linux_target_reuses_verified_linux_archive(self):
        _, _, fetch = self.release()
        with tempfile.TemporaryDirectory() as directory, patch("remote_pair.urllib.request.urlopen", side_effect=fetch) as request:
            binaries = release_binaries(Path(directory), "x86_64-unknown-linux-gnu")
            self.assertEqual(request.call_count, 2)  # metadata and one archive
            self.assertEqual(binaries["controller"]["sha256"], binaries["target"]["sha256"])
            self.assertNotEqual(binaries["controller"]["path"], binaries["target"]["path"])
            self.assertEqual(binaries["target"]["platform"], "x86_64-unknown-linux-gnu")
            self.assertEqual(binaries["target"]["path"].stat().st_mode & 0o777, 0o700)

    def test_macos_default_is_unchanged(self):
        _, _, fetch = self.release()
        with tempfile.TemporaryDirectory() as directory, patch("remote_pair.urllib.request.urlopen", side_effect=fetch):
            binaries = release_binaries(Path(directory))
            self.assertEqual(binaries["target"]["platform"], "aarch64-apple-darwin")
            self.assertNotEqual(binaries["controller"]["sha256"], binaries["target"]["sha256"])

    def test_bad_archive_digest_fails(self):
        release, _, fetch = self.release()
        release["assets"][0]["digest"] = "sha256:" + "0" * 64
        with tempfile.TemporaryDirectory() as directory, patch("remote_pair.urllib.request.urlopen", side_effect=fetch):
            with self.assertRaisesRegex(AssertionError, "checksum mismatch"):
                release_binaries(Path(directory), "x86_64-unknown-linux-gnu")

    def test_github_cli_metadata_preserves_archive_verification(self):
        release, _, fetch = self.release()
        response = SimpleNamespace(returncode=0, stdout=json.dumps(release).encode())
        with tempfile.TemporaryDirectory() as directory, \
                patch("remote_pair.subprocess.run", return_value=response) as gh, \
                patch("remote_pair.urllib.request.urlopen", side_effect=fetch) as request:
            binaries = release_binaries(Path(directory), "x86_64-unknown-linux-gnu", True)
            self.assertEqual(gh.call_args.args[0], ["gh", "api", "repos/trchopan/locho/releases/tags/v1.3.0"])
            self.assertEqual(request.call_count, 1)
            self.assertEqual(binaries["target"]["archive_sha256"], release["assets"][0]["digest"])

    def test_github_cli_error_does_not_expose_stderr(self):
        response = SimpleNamespace(returncode=1, stdout=b"", stderr=b"private diagnostic")
        with tempfile.TemporaryDirectory() as directory, patch("remote_pair.subprocess.run", return_value=response):
            with self.assertRaisesRegex(AssertionError, "GitHub CLI release metadata fetch failed") as raised:
                release_binaries(Path(directory), "x86_64-unknown-linux-gnu", True)
            self.assertNotIn("private diagnostic", str(raised.exception))

    def test_linux_staging_dry_run_is_offline(self):
        result = subprocess.run([sys.executable, "-B", str(Path(__file__).with_name("remote_pair.py")), "both",
            "--profile", "acceptance", "--controller-ssh", "ubuntu@controller.invalid",
            "--target-ssh-host", "target.invalid", "--target-ssh-port", "22", "--target-ssh-user", "root",
            "--target-python", "/usr/bin/python3", "--target-platform", "x86_64-unknown-linux-gnu",
            "--stage-target-binary", "--dry-run"], capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(report["target_port"], 22)
        self.assertEqual(report["target_binary_policy"], "private verified release staging")
        self.assertEqual([phase["schedule"]["duration"] for phase in report["phases"]], [1200, 3600])

    def test_supported_admin_remote_preview_uses_official_four_worker_contract(self):
        result = subprocess.run([sys.executable, "-B", str(Path(__file__).with_name("remote_pair.py")), "both",
            "--profile", "acceptance", "--workload", "supported-admin", "--controller-ssh", "ubuntu@controller.invalid",
            "--target-ssh-host", "target.invalid", "--target-ssh-port", "22", "--target-ssh-user", "root",
            "--target-python", "/usr/bin/python3", "--target-platform", "x86_64-unknown-linux-gnu",
            "--stage-target-binary", "--dry-run"], capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(report["workload"], "supported-admin")
        self.assertEqual(report["suite_role"], "supported_workload_acceptance")
        self.assertFalse(report["product_release_certification"])
        self.assertEqual([phase["schedule"]["concurrency"] for phase in report["phases"]], [4, 4])
        self.assertEqual([phase["schedule"]["expected_bulk_operations"] for phase in report["phases"]], [20, 60])
        self.assertEqual(report["binary_comparison"], "official release")


class TargetStagingTests(unittest.IsolatedAsyncioTestCase):
    def args(self, directory):
        return SimpleNamespace(runtime_parent=directory, target_binary=None, target_sha256=None,
            target_ssh_host="target.invalid", target_ssh_port=22, target_ssh_user="root", target_python="python3",
            target_platform="x86_64-unknown-linux-gnu", stage_target_binary=True, target_staging_parent="/var/tmp",
            controller_ssh="ubuntu@controller.invalid", controller_python="python3", controller_runtime_parent="/var/tmp",
            mode="both", profile="acceptance", relay_url=None)

    def child(self, stdout, code=0):
        return SimpleNamespace(returncode=code, communicate=AsyncMock(return_value=(stdout, b"")))

    async def test_private_target_staging_is_removed_when_stress_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            args = self.args(directory)
            children = [self.child(b'["Linux", "x86_64"]'), self.child(b"verified-hash\n"), self.child(b"{}")]
            binaries = {role: dict(path=Path(directory) / role, sha256="verified-hash") for role in ("controller", "target")}
            with patch("remote_pair.spawn", new=AsyncMock(side_effect=children)) as spawn, \
                    patch("remote_pair.release_binaries", return_value=binaries), \
                    patch("remote_pair.stage_controller", new=AsyncMock(return_value="/var/tmp/lc-owned")) as stage, \
                    patch("remote_pair.run_one", new=AsyncMock(return_value=False)) as run, \
                    patch("builtins.print"):
                self.assertEqual(await main(args), 1)
                self.assertEqual(args.target_binary, "/var/tmp/lc-owned/locho")
                self.assertEqual(run.await_count, 1)
                self.assertEqual(run.call_args.args[1], "stress")
                self.assertIn("root", stage.call_args.kwargs["destination"])
                self.assertIn("/var/tmp/lc-owned", spawn.call_args.args[0][-1])
                self.assertEqual(list(Path(directory).iterdir()), [])


    async def test_wrong_platform_stops_before_download_or_staging(self):
        with tempfile.TemporaryDirectory() as directory:
            args = self.args(directory)
            with patch("remote_pair.spawn", new=AsyncMock(return_value=self.child(b'["Darwin", "arm64"]'))), \
                    patch("remote_pair.release_binaries") as release, \
                    patch("remote_pair.stage_controller", new=AsyncMock()) as stage:
                with self.assertRaisesRegex(AssertionError, "OS/architecture"):
                    await main(args)
                release.assert_not_called()
                stage.assert_not_awaited()

    async def test_wrong_measured_binary_stops_before_load_and_removes_staging(self):
        with tempfile.TemporaryDirectory() as directory:
            args = self.args(directory)
            children = [self.child(b'["Linux", "x86_64"]'), self.child(b"wrong-hash\n"), self.child(b"{}")]
            binaries = {role: dict(path=Path(directory) / role, sha256="verified-hash") for role in ("controller", "target")}
            with patch("remote_pair.spawn", new=AsyncMock(side_effect=children)), \
                    patch("remote_pair.release_binaries", return_value=binaries), \
                    patch("remote_pair.stage_controller", new=AsyncMock(return_value="/var/tmp/lc-owned")), \
                    patch("remote_pair.run_one", new=AsyncMock()) as run, patch("builtins.print"):
                with self.assertRaisesRegex(AssertionError, "measured target source"):
                    await main(args)
                run.assert_not_awaited()
                self.assertEqual(list(Path(directory).iterdir()), [])


class RelayObservationTests(unittest.IsolatedAsyncioTestCase):
    def record(self, monotonic, value, pid=123):
        return dict(process="relay", sample_monotonic=monotonic, pid=pid, uptime_seconds=int(monotonic),
            counters={name: value for name in ("relayserver_bytes_recv_total", "relayserver_bytes_sent_total",
                "relayserver_bytes_rx_ratelimited_total", "relayserver_conns_rx_ratelimited_total",
                "relayserver_send_packets_dropped_total")})

    async def test_rates_use_actual_relay_monotonic_intervals(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor = RelayMonitor(SimpleNamespace(), Artifacts(Path(directory)))
            monitor.child = SimpleNamespace(stdout=None, stdin=SimpleNamespace(write=lambda _: None, drain=AsyncMock()))
            with patch("remote_pair.receive", new=AsyncMock(side_effect=[self.record(10, 100), self.record(12, 300)])):
                await monitor.sample()
                await monitor.sample()
            events = [json.loads(line) for line in (Path(directory) / "metrics-relay.jsonl").read_text().splitlines()]
            self.assertEqual(events[1]["interval_seconds"], 2)
            self.assertEqual(events[1]["bytes_per_second"]["relayserver_bytes_sent_total"], 100)
            self.assertEqual(events[1]["counter_deltas"]["relayserver_send_packets_dropped_total"], 200)

    async def test_restart_or_counter_reset_fails_observation(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor = RelayMonitor(SimpleNamespace(), Artifacts(Path(directory)))
            monitor.child = SimpleNamespace(stdout=None, stdin=SimpleNamespace(write=lambda _: None, drain=AsyncMock()))
            for record in (self.record(12, 200, pid=999), self.record(12, 99)):
                monitor.previous = self.record(10, 100)
                with patch("remote_pair.receive", new=AsyncMock(return_value=record)):
                    with self.assertRaises(AssertionError):
                        await monitor.sample()


class ContinuationTests(unittest.TestCase):
    def test_only_complete_foreground_failure_allows_second_declared_lifetime(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            summary = dict(error=None, cleanup_errors=[], counts={"unexpected_failures": 4},
                           gates={"diagnostic_cases": {"status": "failed"}, "owned_process_cleanup": {"status": "passed"}})
            for name in ("readiness", "same_lifetime_fd_attachment", "same_lifetime_fd_host", "diagnostic_resources_before",
                         "diagnostic_resources_after", "mixed_probe_observation", "mixed_overlap_coverage",
                         "burst_schedule_coverage", "resource_observation_coverage", "conditioning",
                         "persistent_probe_continuity", "application_quiet", "required_metrics", "forwarding_coverage",
                         "transport_observation", "custom_relay_observation", "artifact_transfer"):
                summary["gates"][name] = {"status": "passed"}
            metadata = dict(review={"schedule": {"diagnostic_kind": "transport"}}, diagnostic_cases=[{}, {}, {}])
            admin = dict(target_runtime_removed=True, relay_metrics={"samples": 20, "error": None},
                         errors=["AssertionError: remote workload or cleanup gates failed; inspect summary.json"])
            def write():
                for name, value in (("summary", summary), ("metadata", metadata), ("administration-summary", admin)):
                    (root / (name + ".json")).write_text(json.dumps(value))
            write()
            self.assertTrue(transport_comparison_can_continue(root))
            metadata["review"]["schedule"]["control_wait_seconds"] = 5
            summary["gates"]["no_conditioning_control"] = {"status": "passed"}
            del summary["gates"]["conditioning"]
            write()
            self.assertTrue(transport_comparison_can_continue(root))
            summary["gates"]["owned_process_cleanup"]["status"] = "failed"
            write()
            self.assertFalse(transport_comparison_can_continue(root))
            summary["gates"]["owned_process_cleanup"]["status"] = "passed"
            admin["relay_metrics"]["error"] = "TimeoutError"
            write()
            self.assertFalse(transport_comparison_can_continue(root))

if __name__ == "__main__":
    unittest.main()
