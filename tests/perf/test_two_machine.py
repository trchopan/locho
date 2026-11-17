import argparse
import asyncio
import base64
import hashlib
import json
from pathlib import Path
import shutil
from types import SimpleNamespace
import unittest
import uuid
from unittest.mock import AsyncMock, patch

from native_support import (Artifacts, MESSAGE_LIMIT, custom_relay_observed, encode, environment, message,
                            normalize_relay_url, relay_arguments, same_lifetime_growth, validate_message,
                            TransferProgress, validate_relay_policy, write_relay_config)
from two_machine import Run, SSHControl, SocketControl, admin_bulk_slots, distribution, dry_run, levels, mixed_flows, parser, release_version, schedule, staggered_offsets, RoundStarts
from remote_pair import ControlBridge, phase_profile, workload_options
from unix_streams import Fixture, HEADER, payload, tcp_port


class ProfileTests(unittest.TestCase):
    def args(self, *values):
        return parser().parse_args(values)

    def test_total_concurrency_is_not_multiplied_by_services(self):
        for total in (4, 8, 12, 16, 32, 64, 117):
            workers = distribution(total)
            self.assertEqual(sum(workers.values()), total)
            self.assertEqual(set(workers), {"tt", "tu", "ut", "uu"})
            self.assertLessEqual(max(workers.values()) - min(workers.values()), 1)

    def test_supported_admin_contract_is_distinct_and_preserves_safety_schedules(self):
        for mode, profile, expected in (("stress", "dev", 2), ("soak", "dev", 3),
                                         ("stress", "acceptance", 20), ("soak", "acceptance", 60)):
            args = self.args(mode, "--profile", profile, "--workload", "supported-admin", "--concurrency", "4")
            contract = schedule(args)
            capacity = schedule(self.args(mode, "--profile", profile))
            self.assertEqual(contract["levels"], [4])
            self.assertEqual(contract["concurrency"], 4)
            self.assertEqual(contract["expected_bulk_operations"], expected)
            self.assertEqual(contract["ordinary_payload_bytes"], [1, 256, 16384])
            self.assertEqual(contract["operation_deadline_seconds"], 10)
            self.assertEqual(contract["bulk_max_active"], 1)
            for name in ("duration", "quiet_seconds", "checkpoint", "faults", "setup_budget", "cleanup_budget", "outer_budget"):
                self.assertEqual(contract[name], capacity[name])
            report = dry_run(args, contract, str(uuid.uuid4()))
            self.assertEqual(report["suite_role"], "supported_workload_acceptance")
            self.assertFalse(report["product_release_certification"])
        self.assertEqual(schedule(self.args("stress"))["levels"], [4, 8])
        self.assertEqual(schedule(self.args("stress", "--profile", "acceptance"))["levels"], [4, 8, 16, 32, 64])

    def test_supported_admin_rejects_conflicting_load_overrides(self):
        for values in (("--concurrency", "12"), ("--concurrency", "4", "--levels", "4,8"),
                       ("--concurrency", "4", "--pace", "1")):
            with self.assertRaises(AssertionError):
                schedule(self.args("soak", "--workload", "supported-admin", *values))
        with self.assertRaises(AssertionError):
            schedule(self.args("diagnostic", "--workload", "supported-admin", "--concurrency", "4"))

    def test_bulk_slots_rotate_and_end_before_final_drain(self):
        slots = admin_bulk_slots(3600)
        self.assertEqual(len(slots), 60)
        self.assertEqual([slot["combination"] for slot in slots[:8]], ["tt", "tu", "ut", "uu"] * 2)
        self.assertEqual(slots[-1]["scheduled_offset_seconds"], 3570)
        self.assertEqual([slot["scheduled_offset_seconds"] for slot in admin_bulk_slots(180)], [30, 90, 150])
        self.assertEqual(admin_bulk_slots(45), [])  # no slot exactly at final-drain boundary

    def test_profiles_have_explicit_workload_and_outer_budgets(self):
        for mode, profile, seconds in (("stress", "dev", 120), ("soak", "dev", 180),
                                       ("stress", "acceptance", 1200), ("soak", "acceptance", 3600)):
            result = schedule(self.args(mode, "--profile", profile))
            self.assertEqual(result["duration"], seconds)
            self.assertEqual(result["outer_budget"], 120 + seconds + 30 + 5)
        self.assertEqual(schedule(self.args("soak", "--duration", "2m"))["faults"], [(72, "attachment")])

    def test_short_profiles_still_have_quiet_drain_and_fault_phases(self):
        result = schedule(self.args("soak", "--duration", "2m"))
        self.assertEqual(result["quiet_seconds"], 8)
        self.assertLess(result["quiet_seconds"], result["checkpoint"])
        self.assertLess(result["checkpoint"] + 20, result["faults"][0][0])
        self.assertLess(result["faults"][-1][0] + 30, result["duration"])
        with self.assertRaises(AssertionError):
            schedule(self.args("soak", "--duration", "60s"))
        with self.assertRaises(AssertionError):
            schedule(self.args("stress", "--levels", "4"))
        with self.assertRaises(AssertionError):
            schedule(self.args("soak", "--concurrency", "120"))
        with self.assertRaises(AssertionError):
            schedule(self.args("soak", "--profile", "acceptance", "--duration", "3m"))

    def test_levels_reject_ambiguous_or_unsafe_values(self):
        self.assertEqual(levels("4,8,16"), [4, 8, 16])
        for value in ("1", "4,4", "8,4", "128", "bad"):
            with self.assertRaises(argparse.ArgumentTypeError):
                levels(value)

    def test_release_version_accepts_build_metadata_not_wrong_semver(self):
        self.assertEqual(release_version("locho 1.3.0 (commit abc, clean)"), "1.3.0")
        self.assertEqual(release_version("locho 1.3.0"), "1.3.0")
        self.assertIsNone(release_version("locho 1.3.01oops"))

    def test_smoke_is_bounded_and_has_no_scheduled_fault(self):
        result = schedule(self.args("smoke"))
        self.assertEqual(result["duration"], 20)
        self.assertEqual(result["faults"], [])
        self.assertEqual(result["outer_budget"], 175)

    def test_diagnostic_matrix_is_bounded_and_preserves_deadline(self):
        result = schedule(self.args("diagnostic"))
        self.assertEqual(result["duration"], 150)
        self.assertEqual(result["operation_timeout"], 10)
        self.assertEqual(result["background_workers"] + result["foreground_large_streams"], 12)
        self.assertEqual(result["cases"], ["before_unfragmented", "before_fragmented", "after_unfragmented", "after_fragmented"])
        self.assertEqual(result["faults"], [("after_baseline_cases", "attachment")])
        with self.assertRaises(AssertionError):
            schedule(self.args("diagnostic", "--duration", "60s"))

    def test_duplex_comparison_declares_two_three_and_no_hidden_budget_change(self):
        result = schedule(self.args("diagnostic", "--diagnostic-kind", "duplex"))
        self.assertEqual(result["cases"], ["before_two_duplex", "before_three_duplex", "after_two_duplex", "after_three_duplex"])
        self.assertEqual(result["foreground_large_streams"], [2, 3])
        self.assertEqual(result["background_workers"], [10, 9])
        self.assertEqual(result["operation_timeout"], 10)

    def test_mixed_matrix_has_fixed_budget_no_restarts_and_twelve_total_workers(self):
        args = self.args("diagnostic", "--diagnostic-kind", "mixed")
        result = schedule(args)
        self.assertEqual(result["duration"], 180)
        self.assertEqual(result["faults"], [])
        self.assertEqual(result["foreground_large_streams"], [4, 8, 11])
        self.assertEqual(result["background_workers"], [8, 4, 1])
        self.assertEqual(result["operation_timeout"], 10)
        self.assertTrue(dry_run(args, result, str(uuid.uuid4()))["fixture_observation"])
        self.assertIn("mixed", workload_options(args))
        self.assertEqual(len(result["cases"]), 6)
        for count in (0, 4, 7):
            flows = mixed_flows(count)
            self.assertEqual(sum(duplex for _, _, duplex in flows), count)
            self.assertEqual(len(flows), 4 + count)
            self.assertNotIn(("uu", 2), {(name, index) for name, index, _ in flows})
            self.assertEqual({name: sum(service == name for service, _, _ in flows)
                              for name in ("tt", "tu", "ut", "uu")},
                             {"tt": 1 if count == 0 else 2 if count == 4 else 3,
                              "tu": 1 if count == 0 else 2 if count == 4 else 3,
                              "ut": 1 if count == 0 else 2 if count == 4 else 3,
                              "uu": 1 if count == 0 else 2})
        with self.assertRaises(AssertionError):
            schedule(self.args("diagnostic", "--diagnostic-kind", "mixed", "--duration", "150s"))
        with self.assertRaises(AssertionError):
            mixed_flows(3)

    def test_staggered_comparison_preserves_pair_and_operation_limits(self):
        args = self.args("diagnostic", "--diagnostic-kind", "staggered")
        result = schedule(args)
        self.assertEqual(result["cases"], ["sync_first", "staggered_first", "staggered_second", "sync_second"])
        self.assertEqual(result["duration"], 180)
        self.assertEqual(result["faults"], [])
        self.assertEqual(result["foreground_large_streams"], [11])
        self.assertEqual(result["background_workers"], [1])
        self.assertEqual(result["operation_timeout"], 10)
        self.assertEqual(staggered_offsets(), [0.129, 0.002, 0.132, 0.0, 1.493, 3.533, 6.593, 1.028, 8.810, 4.766, 8.832])
        self.assertTrue(dry_run(args, result, str(uuid.uuid4()))["fixture_observation"])

    def test_transport_comparison_has_three_bursts_conditioning_and_quiet(self):
        args = self.args("diagnostic", "--diagnostic-kind", "transport")
        result = schedule(args)
        self.assertEqual(result["cases"], ["initial_high", "conditioned_high", "post_quiet_high"])
        self.assertEqual(result["duration"], 180)
        self.assertEqual(result["quiet_seconds"], 15)
        self.assertEqual(result["peer_lifetimes"], 2)
        self.assertEqual(result["faults"], [])
        self.assertEqual(result["operation_timeout"], 10)
        self.assertTrue(dry_run(args, result, str(uuid.uuid4()))["fixture_observation"])

    def test_no_conditioning_control_has_zero_explicit_conditioning_and_five_second_wait(self):
        args = self.args("diagnostic", "--diagnostic-kind", "transport", "--no-conditioning-control")
        result = schedule(args)
        self.assertEqual(result["cases"], ["initial_high", "unconditioned_high", "post_quiet_high"])
        self.assertEqual(result["control_wait_seconds"], 5)
        self.assertEqual(result["duration"], 180)
        self.assertEqual(result["operation_timeout"], 10)
        self.assertEqual(result["faults"], [])
        self.assertIn("--no-conditioning-control", workload_options(args))

    def test_soak_fixture_observation_preserves_workload_and_enables_client_telemetry(self):
        plain = self.args("soak")
        observed = self.args("soak", "--fixture-observation")
        self.assertEqual(schedule(plain), schedule(observed))
        report = dry_run(observed, schedule(observed), str(uuid.uuid4()))
        self.assertTrue(report["fixture_observation"])
        self.assertTrue(report["transfer_telemetry"])
        self.assertIn("--fixture-observation", workload_options(observed))

    def test_remote_pair_acceptance_profile_preserves_short_smoke_and_fixed_long_budgets(self):
        args = SimpleNamespace(profile="acceptance")
        self.assertEqual(phase_profile(args, "smoke"), "dev")
        self.assertEqual(phase_profile(args, "stress"), "acceptance")
        self.assertEqual(phase_profile(args, "soak"), "acceptance")
        self.assertEqual(schedule(self.args("stress", "--profile", phase_profile(args, "stress")))["duration"], 1200)
        self.assertEqual(schedule(self.args("soak", "--profile", phase_profile(args, "soak")))["duration"], 3600)
    def test_dry_run_is_offline_and_contains_safety_boundary(self):
        args = self.args("stress", "--dry-run")
        with patch("two_machine.SSHControl.start", side_effect=AssertionError("no SSH")):
            report = dry_run(args, schedule(args), str(uuid.uuid4()))
        self.assertTrue(report["control"]["strict_host_keys"])
        self.assertEqual(report["control"]["port"], 2223)
        self.assertFalse(report["certification"])
        self.assertIn("Offline preview", report["notes"][0])

    def test_descriptor_gate_protects_pid_reuse_and_missing_metrics(self):
        before = dict(pid=10, generation=1, start_identity="first", fds=20)
        self.assertEqual(same_lifetime_growth(before, before | {"fds": 24}, 16), 4)
        for after in (before | {"generation": 2}, before | {"start_identity": "recycled"},
                      before | {"pid": 11}, before | {"fds": None}, before | {"fds": 37}):
            with self.assertRaises(AssertionError):
                same_lifetime_growth(before, after, 16)

    def test_protocol_validates_identity_types_and_size(self):
        run_id = str(uuid.uuid4())
        request = message(run_id, 0, "heartbeat")
        self.assertEqual(validate_message(request, run_id), request)
        for invalid in ([], request | {"version": True}, request | {"run_id": "other"},
                        request | {"id": -1}, request | {"id": True}, request | {"payload": []}):
            with self.assertRaises(AssertionError):
                validate_message(invalid, run_id)
        with self.assertRaises(AssertionError):
            encode({"data": "x" * MESSAGE_LIMIT})


class RelayPolicyTests(unittest.TestCase):
    policy = dict(url="https://relay.example.org:8443", token_env="LOCHO_RELAY_TOKEN",
                  include_n0_relays=False, quic_address_discovery=True)

    def test_policy_is_strict_and_does_not_accept_credentials(self):
        self.assertEqual(validate_relay_policy(self.policy)["url"], "https://relay.example.org:8443/")
        for invalid in (self.policy | {"token": "never-retain"}, self.policy | {"include_n0_relays": 1},
                        self.policy | {"token_env": "bad-name"}, self.policy | {"url": "http://relay.example.org"}):
            with self.assertRaises(AssertionError):
                validate_relay_policy(invalid)
        for value in ("https://private-token@relay.example.org", "https://relay.example.org/?token=secret",
                      "https://relay.example.org/relay", "https://relay.example.org:99999"):
            with self.assertRaises(AssertionError) as error:
                normalize_relay_url(value)
            self.assertNotIn("private-token", str(error.exception))

    def test_only_requested_custom_paths_satisfy_coverage(self):
        self.assertFalse(custom_relay_observed([{"path": "direct(192.0.2.1:4321)"}], self.policy))
        self.assertFalse(custom_relay_observed([{"path": "relay(https://other.example.org/)"}], self.policy))
        self.assertTrue(custom_relay_observed([{"path": "relay(https://relay.example.org:8443/)"}], self.policy))
        self.assertTrue(custom_relay_observed([{"path": "mixed(direct(192.0.2.1:4321), relay(https://relay.example.org:8443/))"}], self.policy))


class ReportingTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        import tempfile
        self.output = tempfile.TemporaryDirectory()
        self.args = parser().parse_args(["soak", "--output", self.output.name])
        self.run = Run(self.args, schedule(self.args), str(uuid.uuid4()))

    async def asyncTearDown(self):
        shutil.rmtree(self.run.root, ignore_errors=True)
        self.output.cleanup()

    async def test_dev_never_certifies_real_idle_or_acceptance(self):
        for name in ("readiness", "integrity_and_steady_state", "forwarding_coverage", "required_metrics",
                     "transport_observation",
                     "same_lifetime_fd_attachment", "same_lifetime_fd_host", "artifact_transfer",
                     "owned_process_cleanup", "dev_quiet", "dev_short_expiry", "recovery_attachment"):
            self.run.gate(name, "passed")
        self.assertTrue(self.run.report(None, []))
        summary = json.loads((self.run.artifacts.root / "summary.json").read_text())
        self.assertFalse(summary["acceptance_certified"])
        self.assertEqual(summary["gates"]["real_365s_quiet"]["status"], "not_exercised")
        self.assertEqual(summary["gates"]["default_300s_expiry"]["status"], "not_exercised")

    async def test_supported_payload_single_bulk_rotation_and_missing_slot_gate(self):
        self.run.args.workload = "supported-admin"
        self.run.args.concurrency = 4
        self.run.profile = schedule(self.run.args)
        self.run.work_started = 0
        clock = SimpleNamespace(monotonic=lambda: 30)
        with patch("two_machine.time", clock), patch.object(self.run, "event"):
            self.assertEqual(self.run.admin_payload("tu", 20), (16384, None))
            size, first = self.run.admin_payload("tt", 20)
            self.assertEqual(size, 1048576)
            clock.monotonic = lambda: 90
            self.assertEqual(self.run.admin_payload("tu", 40), (256, None))
            first["outcome"] = "success"
            self.run.admin_bulk_active = None
            size, second = self.run.admin_payload("tu", 40)
            self.assertEqual(size, 1048576)
            second["outcome"] = "success"
            self.run.admin_bulk_active = None
            with self.assertRaisesRegex(AssertionError, "bulk slots"):
                self.run.admin_bulk_gate()
            self.assertEqual(self.run.gates["supported_bulk_coverage"]["status"], "failed")
            clock.monotonic = lambda: 150
            _, third = self.run.admin_payload("ut", 60)
            third["outcome"] = "success"
            self.run.admin_bulk_active = None
            self.run.admin_bulk_gate()
            self.assertEqual(self.run.gates["supported_bulk_coverage"]["status"], "passed")

    async def test_supported_worker_uses_scheduled_bulk_not_sequence_twenty(self):
        import time
        from unix_streams import exchange as real_exchange, duplex_exchange as real_duplex
        fixture = Fixture(self.run.root / "admin-fixture")
        task = None
        seen = []
        self.run.args.workload = "supported-admin"
        self.run.args.concurrency = 4
        self.run.profile = schedule(self.run.args)
        self.run.args.pace = 0.001  # local regression acceleration only, after contract validation
        self.run.work_started = time.monotonic() - 30
        async def ordinary(reader, writer, identity, sequence, size, **options):
            seen.append((sequence, size, options))
            return await real_exchange(reader, writer, identity, sequence, size, **options)
        async def duplex(reader, writer, identity, sequence, size):
            seen.append((sequence, size, {"duplex": True}))
            return await real_duplex(reader, writer, identity, sequence, size)
        try:
            await fixture.start()
            self.run.listeners = {"tt": fixture.address}
            with patch("two_machine.exchange", ordinary), patch("two_machine.duplex_exchange", duplex):
                task = asyncio.create_task(self.run.worker("tt", 0))
                async with asyncio.timeout(5):
                    while self.run.counts["success"] < 41:
                        await asyncio.sleep(0.01)
                self.run.stop_workers.set()
                await task
            large = [entry for entry in seen if entry[1] == 1048576]
            self.assertEqual(len(large), 1)
            self.assertEqual(large[0][2], {"fragmented": False, "slow": False, "half_close": False})
            self.assertTrue(all(size <= 16384 for sequence, size, _ in seen if sequence in (20, 40)))
            self.assertEqual(self.run.admin_bulk_results[0]["outcome"], "success")
            self.assertIsNone(self.run.admin_bulk_active)
            self.assertEqual(fixture.errors, 0)
            self.assertEqual(fixture.duplicates, 0)
        finally:
            if task and not task.done():
                task.cancel()
                await asyncio.gather(task, return_exceptions=True)
            await fixture.stop()

    async def test_supported_bulk_failure_or_missed_slot_never_replays_or_passes(self):
        self.run.args.workload = "supported-admin"
        self.run.args.concurrency = 4
        self.run.profile = schedule(self.run.args)
        self.run.work_started = 0
        clock = SimpleNamespace(monotonic=lambda: 30)
        with patch("two_machine.time", clock), patch.object(self.run, "event"):
            _, failed = self.run.admin_payload("tt", 20)
            failed["outcome"] = "incomplete"
            self.run.admin_bulk_active = None
            self.assertEqual(self.run.admin_payload("tt", 21), (1, None))
            self.assertEqual(self.run.admin_bulk_next, 1)
            clock.monotonic = lambda: 160
            self.assertEqual(self.run.admin_payload("tu", 40), (256, None))
            self.assertEqual(self.run.admin_bulk_results[1]["outcome"], "missed")
            with self.assertRaises(AssertionError):
                self.run.admin_bulk_gate()
            self.assertEqual(self.run.gates["supported_bulk_coverage"]["status"], "failed")

    async def test_stabilized_baseline_rejects_startup_peak_and_records_selected_sample(self):
        def sample(fds):
            return {"attachment": dict(pid=10, generation=1, start_identity="attachment-start", fds=fds),
                    "host": dict(pid=20, generation=1, start_identity="host-start", fds=21)}
        readings = [sample(value) for value in (128, 25, 25, 25)]
        clock = SimpleNamespace(monotonic=unittest.mock.Mock(side_effect=[100, 100.5, 102, 103.5, 105]))
        with patch("two_machine.time", clock), patch("two_machine.asyncio.sleep", AsyncMock()), \
                patch.object(self.run, "sample", AsyncMock(side_effect=readings)):
            baseline = await self.run.stabilized_fd_baseline()
        self.assertEqual(baseline["attachment"]["fds"], 25)
        review = self.run.metadata["warmed_fd_baseline"]
        self.assertEqual([row["fds"]["attachment"] for row in review["samples"]], [128, 25, 25, 25])
        self.assertEqual(review["status"], "passed")
        self.assertEqual(self.run.gates["warmed_fd_baseline"]["samples"], 4)

    async def test_unstable_fd_baseline_fails_without_choosing_an_arbitrary_minimum(self):
        readings = [{"attachment": dict(pid=10, generation=1, start_identity="a", fds=25 + index % 2),
                     "host": dict(pid=20, generation=1, start_identity="h", fds=21)} for index in range(10)]
        clock = SimpleNamespace(monotonic=unittest.mock.Mock(side_effect=range(11)))
        with patch("two_machine.time", clock), patch("two_machine.asyncio.sleep", AsyncMock()), \
                patch.object(self.run, "sample", AsyncMock(side_effect=readings)):
            with self.assertRaisesRegex(AssertionError, "did not stabilize"):
                await self.run.stabilized_fd_baseline()
        self.assertEqual(self.run.gates["warmed_fd_baseline"]["status"], "failed")
        self.assertNotIn("selected", self.run.metadata["warmed_fd_baseline"])

    async def test_baseline_stabilization_cannot_cross_process_lifetimes(self):
        first = {"attachment": dict(pid=10, generation=1, start_identity="a", fds=25),
                 "host": dict(pid=20, generation=1, start_identity="h", fds=21)}
        second = first | {"attachment": first["attachment"] | {"generation": 2}}
        with patch("two_machine.asyncio.sleep", AsyncMock()), \
                patch.object(self.run, "sample", AsyncMock(side_effect=[first, second])):
            with self.assertRaisesRegex(AssertionError, "lifetime changed"):
                await self.run.stabilized_fd_baseline()
        self.assertEqual(self.run.gates["warmed_fd_baseline"]["status"], "failed")

    async def test_missing_required_gates_and_unverified_cleanup_fail(self):
        self.assertFalse(self.run.report(None, []))
        self.assertEqual(self.run.gates["owned_process_cleanup"]["status"], "not_exercised")

    async def test_redaction_and_caps_preserve_aggregate_failures(self):
        artifacts = self.run.artifacts
        artifacts.secrets.add('private"token')
        artifacts.runtime_paths.add("/private/run")
        artifacts.json("redaction.json", {"secret": 'private"token', "count": 1, "path": "/private/run"})
        record = json.loads((artifacts.root / "redaction.json").read_text())
        self.assertEqual(record["secret"], "<redacted>")
        self.assertEqual(record["count"], 1)
        self.assertEqual(record["path"], "/private/run")
        for _ in range(12):
            self.run.counts["unexpected_failures"] += 1
            artifacts.event("operations.jsonl", {"failure": True}, cap=2)
        self.assertEqual(self.run.counts["unexpected_failures"], 12)
        self.assertEqual(artifacts.truncated["operations.jsonl"], 10)

    async def test_pre_timestamped_transport_events_do_not_kill_log_pumps(self):
        self.run.artifacts.event("transport.jsonl", {"utc": 123.5, "path": "relay(example)"})
        record = json.loads((self.run.artifacts.root / "transport.jsonl").read_text())
        self.assertEqual(record["utc"], 123.5)
        self.assertIn("monotonic", record)

    async def test_private_relay_config_contains_no_bearer_value(self):
        policy = RelayPolicyTests.policy
        with self.assertRaises(AssertionError):
            write_relay_config(self.run.root, policy, {})
        path = write_relay_config(self.run.root, policy, {"LOCHO_RELAY_TOKEN": "private-bearer-value"})
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertNotIn("private-bearer-value", path.read_text())
        self.assertEqual(relay_arguments(path), ["--relay-config", str(path)])
        self.assertEqual(relay_arguments(None), [])

    async def test_approved_environment_is_sourced_privately_and_redacted(self):
        path = self.run.root / "source.env"
        path.write_text("export LOCHO_RELAY_TOKEN='private-bearer-value'\n")
        path.chmod(0o600)
        env = await environment(self.run.root, path, self.run.artifacts)
        self.assertEqual(env["LOCHO_RELAY_TOKEN"], "private-bearer-value")
        self.assertEqual(self.run.artifacts.redact("private-bearer-value"), "<redacted>")
        self.assertEqual(self.run.artifacts.redact("locho 1.3.0"), "locho 1.3.0")
        path.chmod(0o644)
        with self.assertRaises(AssertionError):
            await environment(self.run.root, path, self.run.artifacts)

    async def test_direct_only_custom_profile_does_not_claim_relay_coverage(self):
        self.run.relay_policy = RelayPolicyTests.policy
        self.run.metadata["transport_history_controller"] = [{"path": "direct(192.0.2.1:4321)"}]
        with self.assertRaises(AssertionError):
            self.run.transport_gates()
        self.assertEqual(self.run.gates["custom_relay_observation"]["status"], "not_exercised")

    async def test_smoke_checks_four_combinations_then_one_large_frame(self):
        fixture = Fixture(self.run.root / "smoke-fixture")
        before = dict(pid=10, generation=1, start_identity="owned", fds=20)
        try:
            await fixture.start()
            self.run.args.mode = "smoke"
            self.run.profile = schedule(self.run.args)
            self.run.listeners = {name: fixture.address for name in ("tt", "tu", "ut", "uu")}
            self.run.baseline = {"attachment": before, "host": before}
            self.run.sample = AsyncMock(return_value=self.run.baseline)
            self.run.metadata["transport_history_controller"] = [{"path": "relay(https://relay.example.org:8443/)"}]
            self.run.relay_policy = RelayPolicyTests.policy
            await self.run.workload()
            self.assertEqual(self.run.counts["success"], 5)
            self.assertEqual(self.run.counts["bytes"], 2099200)
            self.assertEqual(self.run.gates["custom_relay_observation"]["status"], "passed")
            self.assertEqual(fixture.errors, 0)
            self.assertEqual(fixture.duplicates, 0)
        finally:
            await fixture.stop()

    async def test_fresh_probe_attempts_never_reuse_worker_identity(self):
        fixture = Fixture(self.run.root / "probe-fixture")
        try:
            await fixture.start()
            self.run.listeners = {name: fixture.address for name in ("tt", "tu", "ut", "uu")}
            await self.run.probe_all()
            await self.run.probe_all()
            self.assertEqual(len(fixture.highwater), 8)
            self.assertEqual(set(fixture.highwater.values()), {1})
            self.assertEqual(fixture.duplicates, 0)
        finally:
            await fixture.stop()

    async def test_aborted_response_tail_is_not_counted_as_corruption(self):
        fixture = Fixture(self.run.root / "abort-fixture")
        entered = asyncio.Event()
        async def stalled_exchange(reader, writer, identity, sequence, size, **options):
            data = payload(64, identity, sequence)
            writer.write(HEADER.pack(b"LUT1", 0, identity, sequence, 64) + data)
            await writer.drain()
            entered.set()
            await asyncio.Event().wait()  # valid echoed frame remains unread
        task = None
        try:
            await fixture.start()
            self.run.listeners = {"tt": fixture.address}
            with patch("two_machine.exchange", side_effect=stalled_exchange):
                task = asyncio.create_task(self.run.worker("tt", 0))
                await asyncio.wait_for(entered.wait(), 3)
                async with asyncio.timeout(3):
                    while fixture.messages == 0:
                        await asyncio.sleep(0.01)
                task.cancel()
                await asyncio.gather(task, return_exceptions=True)
            self.assertEqual(self.run.counts["aborted_operations"], 1)
            self.assertEqual(self.run.counts["validation_failures"], 0)
            self.assertEqual(fixture.errors, 0)
        finally:
            if task and not task.done():
                task.cancel()
                await asyncio.gather(task, return_exceptions=True)
            await fixture.stop()

    async def test_telemetry_reports_partial_read_without_consuming_buffer(self):
        reader = asyncio.StreamReader()
        reader.feed_data(b"h" * HEADER.size + b"partial")
        trace = TransferProgress(64)
        observed, _ = trace.bind(reader, SimpleNamespace())
        await observed.readexactly(HEADER.size)
        with self.assertRaises(asyncio.TimeoutError):
            await asyncio.wait_for(observed.readexactly(64), 0.01)
        snapshot = trace.snapshot()
        self.assertEqual(snapshot["read_phase"], "response_payload")
        self.assertFalse(snapshot["read_wait_completed"])
        self.assertEqual(snapshot["response_header_consumed"], HEADER.size)
        self.assertEqual(snapshot["response_payload_consumed"], 0)
        self.assertEqual(snapshot["buffered_response_bytes"], 7)
        self.assertEqual(await reader.readexactly(7), b"partial")
        self.assertFalse(snapshot["frame_verified"])

    async def test_telemetry_does_not_mistake_queued_bytes_for_drain_completion(self):
        writer = SimpleNamespace(write=lambda _: None, drain=AsyncMock(side_effect=lambda: asyncio.Event().wait()),
                                 transport=SimpleNamespace(get_write_buffer_size=lambda: 1057))
        # AsyncMock returning an awaitable does not itself await that awaitable;
        # use an actual blocking coroutine for cancellation coverage.
        async def blocked():
            await asyncio.Event().wait()
        writer.drain = blocked
        trace = TransferProgress(1024)
        _, observed = trace.bind(asyncio.StreamReader(), writer)
        observed.write(b"x" * (HEADER.size + 1024))
        with self.assertRaises(asyncio.TimeoutError):
            await asyncio.wait_for(observed.drain(), 0.01)
        snapshot = trace.snapshot()
        self.assertEqual(snapshot["request_payload_queued"], 1024)
        self.assertEqual(snapshot["local_drain_completed_bytes"], 0)
        self.assertFalse(snapshot["write_wait_completed"])
        self.assertEqual(snapshot["read_phase"], "not_started")

    async def test_diagnostic_transfer_preserves_fragment_sizes_and_validates_frame(self):
        fixture = Fixture(self.run.root / "diagnostic-fixture")
        try:
            await fixture.start()
            self.run.attachment = SimpleNamespace(generation=1)
            self.run.listeners = {"uu": fixture.address}
            record = await self.run.diagnostic_transfer("before_fragmented", 0, "uu", True, False)
            self.assertEqual(record["outcome"], "success")
            self.assertEqual(record["sequence"], 4)
            self.assertTrue(record["telemetry"]["frame_verified"])
            self.assertEqual(record["telemetry"]["request_payload_queued"], 1048576)
            self.assertEqual(record["telemetry"]["response_payload_consumed"], 1048576)
            self.assertEqual(record["telemetry"]["drain_calls"], 1053)
            self.assertEqual(record["telemetry"]["read_phase"], "response_eof")
            self.assertEqual(fixture.errors, 0)
        finally:
            await fixture.stop()

    async def test_diagnostic_case_keeps_failure_with_fresh_independent_results(self):
        records = [dict(combination=name, outcome="failed" if i == 0 else "success", identity=uuid.uuid4().hex,
                        telemetry={"frame_verified": i != 0})
                   for i, name in enumerate(("tt", "ut", "uu", "ut", "uu"))]
        fixtures = {name: {"errors": 0, "duplicates": 0, "highwater": {}} for name in ("tt", "ut", "unix")}
        self.run.control = SimpleNamespace(rpc=AsyncMock(return_value={"fixtures": fixtures}))
        async def sampler(stopped):
            await stopped.wait()
        with patch.object(self.run, "diagnostic_transfer", AsyncMock(side_effect=records)), \
             patch.object(self.run, "diagnostic_sampling", sampler):
            result = await self.run.diagnostic_case("before_fragmented", True)
        self.assertFalse(result["passed"])
        self.assertEqual(len(result["flows"]), 5)
        self.assertEqual(self.run.metadata["diagnostic_cases"][0]["flows"][0]["outcome"], "failed")

    async def test_duplex_background_adjustment_keeps_common_connections_active(self):
        async def worker(name, index, *, stopped, **options):
            while not stopped.is_set():
                await asyncio.sleep(0.001)
        try:
            with patch.object(self.run, "worker", worker):
                await self.run.set_duplex_background(2, reset=True)
                await asyncio.sleep(0)
                initial = dict(self.run.diagnostic_background)
                await self.run.set_duplex_background(3)
                self.assertEqual(len(self.run.tasks), 9)
                self.assertTrue(initial["tu", 1][0].done())
                for key in self.run.diagnostic_background:
                    self.assertIs(self.run.diagnostic_background[key][0], initial[key][0])
                await self.run.set_duplex_background(2)
                self.assertEqual(len(self.run.tasks), 10)
                self.assertIsNot(self.run.diagnostic_background["tu", 1][0], initial["tu", 1][0])
        finally:
            for task in self.run.tasks:
                task.cancel()
            await asyncio.gather(*self.run.tasks, return_exceptions=True)
            self.run.tasks = []

    async def test_mixed_background_keeps_persistent_probe_and_total_distribution(self):
        options_by_key = {}
        async def worker(name, index, *, stopped, **options):
            options_by_key[name, index] = options
            await stopped.wait()
        try:
            with patch.object(self.run, "worker", worker):
                await self.run.set_mixed_background(0, reset=True)
                await asyncio.sleep(0)
                initial_probe = self.run.diagnostic_background["uu", 2][0]
                self.assertTrue(options_by_key["uu", 2]["probe"])
                self.assertEqual(options_by_key["uu", 2]["fixed_size"], 1)
                for count in (4, 7, 7, 4, 0):
                    await self.run.set_mixed_background(count)
                    self.assertEqual(len(self.run.tasks), 8 - count)
                    self.assertIs(self.run.diagnostic_background["uu", 2][0], initial_probe)
                    foreground = {(name, index) for name, index, _ in mixed_flows(count)}
                    self.assertEqual(len(foreground | set(self.run.diagnostic_background)), 12)
        finally:
            for task in self.run.tasks:
                task.cancel()
            await asyncio.gather(*self.run.tasks, return_exceptions=True)
            self.run.tasks = []

    async def test_mixed_probe_does_not_churn_and_retains_checked_telemetry(self):
        fixture = Fixture(self.run.root / "probe-fixture")
        stopped = asyncio.Event()
        task = None
        try:
            await fixture.start()
            self.run.listeners = {"uu": fixture.address}
            self.run.args.diagnostic_kind = "mixed"
            self.run.args.transfer_telemetry = True
            self.run.args.pace = 0.01
            self.run.mixed_case = "C_high_first"
            task = asyncio.create_task(self.run.worker("uu", 2, small_only=True, fixed_size=1,
                                                      stopped=stopped, probe=True))
            async with asyncio.timeout(3):
                while self.run.counts["diagnostic_probe_success"] < 8:
                    await asyncio.sleep(0.01)
            stopped.set()
            await task
            self.assertEqual(fixture.accepted, 1)
            self.assertEqual(fixture.errors, 0)
            self.assertGreaterEqual(len(self.run.mixed_probe_events), 8)
            events = [json.loads(line) for line in (self.run.artifacts.root / "operations.jsonl").read_text().splitlines()]
            self.assertTrue(all(event["probe"] and event["case"] == "C_high_first" for event in events))
            self.assertTrue(all(event["telemetry"]["frame_verified"] for event in events))
        finally:
            if task and not task.done():
                task.cancel()
                await asyncio.gather(task, return_exceptions=True)
            await fixture.stop()

    async def test_mixed_high_round_validates_eleven_synchronized_large_flows(self):
        fixture = Fixture(self.run.root / "mixed-fixture")
        fixtures = {name: {"errors": 0, "duplicates": 0, "highwater": {}} for name in ("tt", "ut", "unix")}
        async def sampler(stopped):
            await stopped.wait()
        try:
            await fixture.start()
            self.run.listeners = {name: fixture.address for name in ("tt", "tu", "ut", "uu")}
            self.run.attachment = SimpleNamespace(generation=1)
            self.run.args.diagnostic_kind = "mixed"
            self.run.control = SimpleNamespace(rpc=AsyncMock(return_value={"fixtures": fixtures}))
            with patch.object(self.run, "diagnostic_sampling", sampler):
                result = await self.run.diagnostic_case("C_high_first", mixed_duplex_count=7)
            self.assertTrue(result["passed"])
            self.assertEqual(result["observed_peak_large_operations"], 11)
            self.assertEqual(len({record["identity"] for record in result["flows"]}), 11)
            self.assertTrue(all(record["telemetry"]["frame_verified"] for record in result["flows"]))
            self.assertEqual(fixture.messages, 44)  # three warmups and one large frame per fresh ID
            self.assertEqual(fixture.errors, 0)
            self.assertEqual(fixture.duplicates, 0)
            self.assertIsNone(self.run.mixed_case)
        finally:
            await fixture.stop()

    async def test_scheduled_start_uses_shared_epoch_and_waits_outside_operation_timeout(self):
        fixture = Fixture(self.run.root / "scheduled-fixture")
        try:
            await fixture.start()
            self.run.listeners = {"uu": fixture.address}
            self.run.attachment = SimpleNamespace(generation=1)
            self.run.args.diagnostic_kind = "staggered"
            record = await self.run.diagnostic_transfer("staggered_first", 0, "uu", False, False,
                                                        RoundStarts(1), start_offset=0.1)
            self.assertEqual(record["outcome"], "success")
            self.assertGreaterEqual(record["actual_start_offset_seconds"], 0.1)
            self.assertLess(record["start_lateness_seconds"], 0.25)
            self.assertEqual(record["operation_timeout"], 10)
            self.assertEqual(record["sequence"], 4)
            self.assertTrue(record["telemetry"]["frame_verified"])
            self.assertLess(record["telemetry"]["elapsed_seconds"], record["actual_start_offset_seconds"])
        finally:
            await fixture.stop()

    async def test_round_barrier_has_one_monotonic_epoch(self):
        barrier = RoundStarts(11)
        epochs = await asyncio.gather(*(barrier.wait() for _ in range(11)))
        self.assertEqual(len(set(epochs)), 1)

    async def test_probe_pause_retains_one_checked_connection_across_quiet(self):
        fixture = Fixture(self.run.root / "quiet-probe")
        stopped = asyncio.Event()
        task = None
        try:
            await fixture.start()
            self.run.listeners = {"uu": fixture.address}
            self.run.args.pace = 0.01
            task = asyncio.create_task(self.run.worker("uu", 2, small_only=True, fixed_size=1,
                                                      stopped=stopped, probe=True))
            async with asyncio.timeout(3):
                while self.run.counts["diagnostic_probe_success"] < 2:
                    await asyncio.sleep(0.01)
            self.run.probe_resume.clear()
            self.run.probe_pause_requested.set()
            await asyncio.wait_for(self.run.probe_paused.wait(), 2)
            before = self.run.counts["diagnostic_probe_success"]
            await asyncio.sleep(0.05)
            self.assertEqual(self.run.counts["diagnostic_probe_success"], before)
            self.run.probe_pause_requested.clear()
            self.run.probe_resume.set()
            async with asyncio.timeout(3):
                while self.run.counts["diagnostic_probe_success"] <= before:
                    await asyncio.sleep(0.01)
            stopped.set()
            await task
            self.assertEqual(self.run.probe_connections, 1)
            self.assertEqual(fixture.accepted, 1)
        finally:
            self.run.probe_resume.set()
            if task and not task.done():
                task.cancel()
                await asyncio.gather(task, return_exceptions=True)
            await fixture.stop()

    async def test_transport_orders_conditioning_and_quiet_without_restart(self):
        self.run.args.mode = "diagnostic"
        self.run.args.diagnostic_kind = "transport"
        self.run.profile = schedule(self.run.args)
        self.run.probe_connections = 1
        self.run.counts["diagnostic_probe_success"] = 2
        self.run.resource_coverage = {name: dict(cpu_intervals=2, cgroup_samples=2, socket_samples=2)
                                      for name in ("attachment", "host", "fixtures")}
        ordered = []
        async def conditioning():
            ordered.append("conditioning")
            self.run.gate("conditioning", "passed")
        async def quiet():
            ordered.append("quiet")
            self.run.gate("application_quiet", "passed")
        async def case(label, **options):
            ordered.append(label)
            self.run.metadata.setdefault("diagnostic_cases", []).append(dict(case=label, passed=True,
                duplex_large_streams=7, observed_peak_large_operations=11,
                probe={"success_during_large_window": 1}, flows=[dict(large_transfer_exercised=True,
                    start_lateness_seconds=0.001) for _ in range(11)]))
        with patch.object(self.run, "resource_gate", AsyncMock()), \
                patch.object(self.run, "set_mixed_background", AsyncMock()), \
                patch.object(self.run, "transport_conditioning", conditioning), \
                patch.object(self.run, "transport_quiet", quiet), \
                patch.object(self.run, "diagnostic_case", AsyncMock(side_effect=case)) as cases, \
                patch.object(self.run, "fault", AsyncMock()) as fault, patch.object(self.run, "transport_gates"):
            await self.run.mixed_diagnostic()
        self.assertEqual(ordered, ["initial_high", "conditioning", "conditioned_high", "quiet", "post_quiet_high"])
        self.assertTrue(all(call.kwargs["offsets"] == [0.0] * 11 for call in cases.call_args_list))
        self.assertEqual(self.run.gates["persistent_probe_continuity"]["status"], "passed")
        fault.assert_not_awaited()

    async def test_transport_quiet_excludes_application_silence_from_load_time(self):
        self.run.probe_paused.set()
        self.run.load_interval_started = 90
        clock = SimpleNamespace(monotonic=unittest.mock.Mock(side_effect=[100, 100, 114.99, 115.1, 115.2, 116]))
        with patch("two_machine.time", clock), patch.object(self.run, "sample", AsyncMock()), \
                patch.object(self.run, "event"):
            await self.run.transport_quiet()
        self.assertEqual(self.run.load_intervals, [10])
        self.assertEqual(self.run.load_interval_started, 116)
        self.assertTrue(self.run.probe_resume.is_set())
        self.assertFalse(self.run.probe_pause_requested.is_set())

    async def test_no_conditioning_wait_generates_no_foreground_operations(self):
        async def sampler(stopped):
            await stopped.wait()
        with patch.object(self.run, "diagnostic_sampling", sampler), \
                patch("two_machine.asyncio.sleep", AsyncMock()) as sleep, \
                patch.object(self.run, "diagnostic_transfer", AsyncMock()) as transfer:
            await self.run.transport_control_wait()
        sleep.assert_awaited_once_with(5)
        transfer.assert_not_awaited()
        self.assertEqual(self.run.metadata["conditioning"], [])
        self.assertEqual(self.run.gates["no_conditioning_control"]["explicit_conditioning_large_transfers"], 0)
        self.assertIsNone(self.run.mixed_case)

    async def test_no_conditioning_orders_wait_instead_of_conditioning(self):
        self.run.args.mode = "diagnostic"
        self.run.args.diagnostic_kind = "transport"
        self.run.args.no_conditioning_control = True
        self.run.profile = schedule(self.run.args)
        self.run.probe_connections = 1
        self.run.counts["diagnostic_probe_success"] = 2
        self.run.resource_coverage = {name: dict(cpu_intervals=2, cgroup_samples=2, socket_samples=2)
                                      for name in ("attachment", "host", "fixtures")}
        ordered = []
        async def control():
            ordered.append("control_wait")
            self.run.gate("no_conditioning_control", "passed")
        async def quiet():
            ordered.append("quiet")
            self.run.gate("application_quiet", "passed")
        async def case(label, **options):
            ordered.append(label)
            self.run.metadata.setdefault("diagnostic_cases", []).append(dict(case=label, passed=True,
                duplex_large_streams=7, observed_peak_large_operations=11,
                probe={"success_during_large_window": 1}, flows=[dict(large_transfer_exercised=True,
                    start_lateness_seconds=0.001) for _ in range(11)]))
        with patch.object(self.run, "resource_gate", AsyncMock()), \
                patch.object(self.run, "set_mixed_background", AsyncMock()), \
                patch.object(self.run, "transport_conditioning", AsyncMock()) as conditioning, \
                patch.object(self.run, "transport_control_wait", control), \
                patch.object(self.run, "transport_quiet", quiet), \
                patch.object(self.run, "diagnostic_case", AsyncMock(side_effect=case)), \
                patch.object(self.run, "transport_gates"):
            await self.run.mixed_diagnostic()
        self.assertEqual(ordered, ["initial_high", "control_wait", "unconditioned_high", "quiet", "post_quiet_high"])
        conditioning.assert_not_awaited()
        self.run.report(None, [])
        self.assertNotIn("conditioning", self.run.gates)

    async def test_staggered_rounds_do_not_require_forced_eleven_way_overlap(self):
        self.run.args.mode = "diagnostic"
        self.run.args.diagnostic_kind = "staggered"
        self.run.profile = schedule(self.run.args)
        self.run.resource_coverage = {name: dict(cpu_intervals=2, cgroup_samples=2, socket_samples=2)
                                      for name in ("attachment", "host", "fixtures")}
        async def case(label, *, mixed_duplex_count, offsets):
            self.run.metadata.setdefault("diagnostic_cases", []).append(dict(case=label, passed=True,
                duplex_large_streams=7, observed_peak_large_operations=4 if label.startswith("staggered") else 11,
                probe={"success_during_large_window": 1}, flows=[dict(large_transfer_exercised=True,
                    start_lateness_seconds=0.001) for _ in range(11)]))
        with patch.object(self.run, "resource_gate", AsyncMock()), \
                patch.object(self.run, "set_mixed_background", AsyncMock()), \
                patch.object(self.run, "diagnostic_case", AsyncMock(side_effect=case)) as cases, \
                patch.object(self.run, "fault", AsyncMock()) as fault, patch.object(self.run, "transport_gates"):
            await self.run.mixed_diagnostic()
        self.assertEqual(cases.await_count, 4)
        self.assertEqual(cases.call_args_list[1].kwargs["offsets"], staggered_offsets())
        self.assertEqual(cases.call_args_list[0].kwargs["offsets"], [0.0] * 11)
        self.assertEqual(self.run.gates["mixed_overlap_coverage"]["status"], "passed")
        self.assertEqual(self.run.gates["staggered_schedule_coverage"]["status"], "passed")
        fault.assert_not_awaited()

    async def test_mixed_round_failure_is_retained_without_restart_or_retry(self):
        self.run.args.mode = "diagnostic"
        self.run.args.diagnostic_kind = "mixed"
        self.run.profile = schedule(self.run.args)
        async def case(label, *, mixed_duplex_count):
            result = dict(case=label, passed=label != "A_ordinary_first",
                          duplex_large_streams=mixed_duplex_count, observed_peak_large_operations=4 + mixed_duplex_count,
                          probe={"success_during_large_window": 1}, flows=[])
            self.run.metadata.setdefault("diagnostic_cases", []).append(result)
            if not result["passed"]:
                self.run.counts["unexpected_failures"] += 1
            return result
        with patch.object(self.run, "resource_gate", AsyncMock()), \
                patch.object(self.run, "set_mixed_background", AsyncMock()), \
                patch.object(self.run, "diagnostic_case", AsyncMock(side_effect=case)) as cases, \
                patch.object(self.run, "fault", AsyncMock()) as fault, \
                patch.object(self.run, "transport_gates"):
            await self.run.mixed_diagnostic()
        self.assertEqual([call.args[0] for call in cases.call_args_list], self.run.profile["cases"])
        self.assertEqual(cases.await_count, 6)
        fault.assert_not_awaited()
        self.assertEqual(self.run.gates["diagnostic_cases"]["status"], "failed")
        self.assertEqual(self.run.gates["mixed_overlap_coverage"]["status"], "passed")
        self.assertFalse(self.run.report(None, []))
        self.assertNotIn("recovery_attachment", self.run.gates)

    async def test_active_frame_capture_keeps_identity_sequence_and_phase_without_replay(self):
        trace = TransferProgress(1048576)
        trace.bind(asyncio.StreamReader(), SimpleNamespace())
        self.run.active_transfers["worker", "uu", 2] = dict(combination="uu", worker=2,
                identity="generated-test-id", sequence=140, operation_epoch=0, trace=trace)
        self.run.fault_generation = 1
        captured = self.run.capture_active_transfers("fault_start")
        self.assertEqual(captured[0]["sequence"], 140)
        self.assertEqual(captured[0]["operation_epoch"], 0)
        self.assertEqual(captured[0]["current_fault_epoch"], 1)
        self.assertFalse(captured[0]["telemetry"]["frame_verified"])
        self.assertEqual(len(self.run.active_transfers), 1)
        self.assertNotIn("trace", captured[0])
        self.assertEqual(self.run.counts["success"], 0)

    async def test_partial_artifact_transfer_never_passes(self):
        digest = hashlib.sha256(b"expected").hexdigest()
        self.run.control = SimpleNamespace(rpc=AsyncMock(side_effect=[
            {"log": {"size": 8, "sha256": digest}}, {"data": base64.b64encode(b"bad").decode()},
            {"data": ""}]))
        with self.assertRaises(AssertionError):
            await self.run.collect()
        self.assertFalse(self.run.collected)

    async def test_artifact_manifest_cannot_escape_output(self):
        self.run.control = SimpleNamespace(rpc=AsyncMock(return_value={
            "../../foreign": {"size": 0, "sha256": hashlib.sha256(b"").hexdigest()}}))
        with self.assertRaises(AssertionError):
            await self.run.collect()

    async def test_protocol_response_mismatch_is_rejected(self):
        control = SSHControl(self.args, self.run.run_id, self.run.profile)
        reader = asyncio.StreamReader()
        reader.feed_data(encode({"version": 1, "run_id": "other", "id": 1, "ok": True, "payload": {}}))
        control.child = SimpleNamespace(returncode=None, stdout=reader,
                                        stdin=SimpleNamespace(write=lambda _: None, drain=AsyncMock()))
        with self.assertRaises(AssertionError):
            await control.rpc("heartbeat")

    async def test_workers_exercise_framed_modes_without_replay(self):
        fixtures = {"tt": Fixture(("127.0.0.1", tcp_port())),
                    "ut": Fixture(("127.0.0.1", tcp_port())),
                    "unix": Fixture(self.run.root / "fixture")}
        try:
            for fixture in fixtures.values():
                await fixture.start()
            self.run.listeners = dict(tt=fixtures["tt"].address[1], ut=fixtures["ut"].address[1],
                                      tu=fixtures["unix"].address, uu=fixtures["unix"].address)
            self.run.args.pace = 0.01
            self.run.tasks = [asyncio.create_task(self.run.worker(name, 2)) for name in self.run.listeners]
            async with asyncio.timeout(15):
                while any(self.run.counts[f"{name}.success"] < 41 for name in self.run.listeners):
                    for task in self.run.tasks:
                        if task.done():
                            task.result()
                    await asyncio.sleep(0.01)
            await self.run.drain()
            self.assertEqual(self.run.counts["unexpected_failures"], 0)
            self.assertEqual(self.run.counts["validation_failures"], 0)
            for fixture in fixtures.values():
                self.assertEqual(fixture.duplicates, 0)
                self.assertEqual(fixture.errors, 0)
        finally:
            for task in self.run.tasks:
                task.cancel()
            await asyncio.gather(*self.run.tasks, return_exceptions=True)
            for fixture in fixtures.values():
                await fixture.stop()

    async def test_socket_bridge_verifies_finish_and_supports_reconnect(self):
        backend = SimpleNamespace(root="/private/generated-target", broken=False,
                                  child=SimpleNamespace(returncode=0, wait=AsyncMock(return_value=0)),
                                  start=AsyncMock(), reconnect=AsyncMock(), close=AsyncMock(),
                                  rpc=AsyncMock(return_value={"cleanup": "verified"}))
        backend.child.returncode = None
        bridge = ControlBridge(backend, self.run.run_id)
        socket = self.run.root / "control"
        server = await asyncio.start_unix_server(bridge.client, str(socket))
        control = SocketControl(SimpleNamespace(control_socket=str(socket)), self.run.run_id, self.run.profile)
        try:
            await control.start()
            self.assertEqual(control.root, backend.root)
            await control.reconnect()
            with self.assertRaises(AssertionError):
                await control.wait()
            backend.child.returncode = 0
            await control.rpc("finish")
            self.assertTrue(bridge.finished)
            self.assertEqual(await control.wait(), 0)
        finally:
            await control.close()
            server.close()
            await server.wait_closed()
            await bridge.close()


if __name__ == "__main__":
    unittest.main()
