import asyncio
from pathlib import Path
import shutil
import sys
import tempfile
from types import SimpleNamespace
import unittest
import uuid

from unix_streams import (
    Fixture, HEADER, LOG_LIMIT, MAX_EVENTS, MAX_PAYLOAD, Process, Run, connect, close, duration,
    duplex_exchange, exchange, fault_failure, payload, short_root, write_toml,
)


class FixtureTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.root = short_root()
        self.fixture = Fixture(self.root / "u")
        await self.fixture.start()

    async def asyncTearDown(self):
        await self.fixture.stop()
        self.root.rmdir()

    async def test_binary_round_trip_and_half_close(self):
        for half_close in (False, True):
            reader, writer = await connect(self.fixture.address)
            try:
                async with asyncio.timeout(5):
                    await exchange(reader, writer, uuid.uuid4().bytes, 1, 65536,
                                   fragmented=True, half_close=half_close)
            finally:
                await close(writer)
        self.assertEqual(self.fixture.messages, 2)
        self.assertEqual(self.fixture.errors, 0)

    async def test_duplicate_sequence_is_rejected(self):
        reader, writer = await connect(self.fixture.address)
        identity = uuid.uuid4().bytes
        try:
            await exchange(reader, writer, identity, 1, 256)
            with self.assertRaises(asyncio.IncompleteReadError):
                await asyncio.wait_for(exchange(reader, writer, identity, 1, 256), 2)
            self.assertEqual(self.fixture.duplicates, 1)
        finally:
            await close(writer)

    async def test_full_duplex_large_transfer(self):
        reader, writer = await connect(self.fixture.address)
        try:
            await asyncio.wait_for(duplex_exchange(reader, writer, uuid.uuid4().bytes,
                                                 1, 2 * 1024 * 1024), 5)
        finally:
            await close(writer)
        self.assertEqual(self.fixture.errors, 0)

    async def test_oversized_frame_is_rejected_without_allocating_payload(self):
        reader, writer = await connect(self.fixture.address)
        writer.write(HEADER.pack(b"LUT1", 0, uuid.uuid4().bytes, 1, MAX_PAYLOAD + 1))
        await writer.drain()
        self.assertEqual(await asyncio.wait_for(reader.read(1), 2), b"")
        self.assertEqual(self.fixture.errors, 1)
        await close(writer)

    async def test_restart_closes_connections_and_retains_replay_accounting(self):
        reader, writer = await connect(self.fixture.address)
        identity = uuid.uuid4().bytes
        await exchange(reader, writer, identity, 1, 1)
        await self.fixture.stop()
        self.assertEqual(await asyncio.wait_for(reader.read(1), 2), b"")
        await close(writer)
        await self.fixture.start()
        reader, writer = await connect(self.fixture.address)
        await exchange(reader, writer, identity, 2, 1)
        self.assertEqual(self.fixture.messages, 2)
        await close(writer)


class HarnessTests(unittest.TestCase):
    def test_fault_accounting_follows_connection_epoch_not_a_grace_window(self):
        # An old persistent connection fails after fresh probes have recovered.
        self.assertTrue(fault_failure(False, False, 1, 0, 1))
        # An operation was in flight when the fault occurred.
        self.assertTrue(fault_failure(False, False, 0, 0, 1))
        self.assertTrue(fault_failure(True, False, 1, 1, 1))
        # A newly opened connection failing after recovery is NOT excused.
        self.assertFalse(fault_failure(False, False, 1, 1, 1))
        self.assertFalse(fault_failure(False, False, 0, 0, 0))

    def test_duration_and_payload_are_deterministic(self):
        self.assertEqual(duration("30m"), 1800)
        self.assertEqual(duration("1h"), 3600)
        self.assertEqual(duration("30"), 30)
        identity = b"0" * 16
        self.assertEqual(payload(1024, identity, 1), payload(1024, identity, 1))
        self.assertNotEqual(payload(1024, identity, 1), payload(1024, identity, 2))
        self.assertEqual(MAX_EVENTS, 2000)

    def test_private_config_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "config"
            write_toml(path, "secret")
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError):
                write_toml(path, "replacement")
            self.assertEqual(path.read_text(), "secret")

    def test_failure_event_cap_does_not_hide_acceptance_gates(self):
        with tempfile.TemporaryDirectory() as root:
            run = SimpleNamespace(output=Path(root), events=0)
            for _ in range(MAX_EVENTS + 10):
                Run.event(run, "operation_failed", expected=True)
            Run.event(run, "quiet_gate_passed")
            lines = (Path(root) / "timeline.jsonl").read_text().splitlines()
            self.assertEqual(len(lines), MAX_EVENTS + 1)
            self.assertEqual(run.events, MAX_EVENTS)
            self.assertIn("quiet_gate_passed", lines[-1])


class ProcessTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.root = short_root()
        self.run = SimpleNamespace(root=self.root, output=self.root, binary=Path(sys.executable),
                                   processes=[], secrets=["private-test-token"])

    async def asyncTearDown(self):
        for process in self.run.processes:
            await process.stop(clean=False)
        shutil.rmtree(self.root)

    async def test_process_logs_redact_credentials_and_runtime_paths(self):
        source = f"print('private-test-token {self.root}')"
        process = await Process(self.run, "redaction", ["-c", source]).start()
        await asyncio.wait_for(process.child.wait(), 5)
        await process.stop()
        log = (self.root / "redaction.log").read_text()
        self.assertIn("<redacted> <runtime>", log)
        self.assertNotIn("private-test-token", log)
        self.assertNotIn(str(self.root), log)

    async def test_process_log_and_readiness_history_are_bounded(self):
        process = await Process(self.run, "bounded", ["-c", "for _ in range(5000): print('x' * 256)"]).start()
        await asyncio.wait_for(process.child.wait(), 10)
        await process.stop()
        self.assertLessEqual((self.root / "bounded.log").stat().st_size, LOG_LIMIT + 512)
        self.assertEqual(len(process.lines), 128)

    async def test_owned_process_receives_sigterm_and_is_reaped(self):
        source = ("import signal, sys, time; "
                  "signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)); "
                  "print('ready', flush=True); time.sleep(60)")
        process = await Process(self.run, "signal", ["-c", source]).start()
        await process.wait_for("ready")
        await process.stop()
        self.assertEqual(process.child.returncode, 0)


if __name__ == "__main__":
    unittest.main()
