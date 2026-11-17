import asyncio
import json
import os
from pathlib import Path
import shutil
import signal
import sys
import time
from types import SimpleNamespace
import unittest
import uuid
from unittest.mock import AsyncMock, patch

from native_support import Artifacts, LOG_LIMIT, OwnedProcess, encode, message, private_json, process_identity, receive
from remote_runner import FixtureObservation, Supervisor
from unix_streams import Fixture, HEADER, close, connect, exchange, payload, short_root


class OwnedProcessTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.root = short_root()
        self.artifacts = Artifacts(self.root / "artifacts")
        self.artifacts.secrets.add("private-test-token")
        self.artifacts.runtime_paths.add(str(self.root))
        self.processes = []

    async def asyncTearDown(self):
        for process in self.processes:
            await process.force_reap()
        shutil.rmtree(self.root)

    async def start(self, source, **kwargs):
        process = OwnedProcess("child", [sys.executable, "-c", source], self.artifacts, **kwargs)
        self.processes.append(process)
        await process.start()
        return process

    async def test_child_limits_and_same_lifetime_metrics(self):
        process = await self.start("import resource,time; print(resource.getrlimit(resource.RLIMIT_NOFILE),flush=True); time.sleep(60)")
        await process.ready("(1024, 4096)")
        sample = await process.sample()
        self.assertEqual(sample["pid"], process.pid)
        self.assertNotEqual(sample["pid"], process.child.pid)
        self.assertIsInstance(sample["fds"], int)
        self.assertGreater(sample["fd_peak"], 0)
        self.assertEqual(sample["generation"], 1)

    async def test_graceful_signal_reaps_binary_and_guard(self):
        process = await self.start("import signal,sys,time; signal.signal(signal.SIGTERM,lambda *_:sys.exit(0)); print('ready',flush=True); time.sleep(60)")
        await process.ready("ready")
        await process.stop()
        self.assertEqual(process.child.returncode, 0)
        with self.assertRaises(AssertionError):
            await process_identity(process.pid)

    async def test_lost_owner_pipe_cleans_child_independently(self):
        process = await self.start("import time; print('ready',flush=True); time.sleep(60)")
        await process.ready("ready")
        os.close(process.owner_fd)
        process.owner_fd = None
        await asyncio.wait_for(process.child.wait(), 5)
        with self.assertRaises(AssertionError):
            await process_identity(process.pid)

    async def test_guard_hard_deadline_kills_term_ignoring_child(self):
        process = await self.start("import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print('ready',flush=True); time.sleep(60)", budget=0.2)
        await process.ready("ready")
        await asyncio.wait_for(process.child.wait(), 5)
        self.assertNotEqual(process.child.returncode, 0)
        with self.assertRaises(AssertionError):
            await process_identity(process.pid)

    async def test_chunked_logs_are_redacted_and_strictly_bounded(self):
        source = (f"import sys,time; sys.stdout.write('private-test-'); sys.stdout.flush(); time.sleep(.1); "
                  f"print('token {self.root}'); print('x'*2000000); time.sleep(.5)")
        process = await self.start(source)
        await asyncio.wait_for(process.child.wait(), 5)
        await process.stop()
        data = (self.artifacts.root / "logs" / "child.log").read_bytes()
        self.assertNotIn(b"private-test-token", data)
        self.assertIn(b"<redacted> <runtime>", data)
        self.assertLessEqual(len(data), LOG_LIMIT)
        self.assertTrue(process.log_truncated)

    async def test_failed_startup_still_has_owned_cleanup_handle(self):
        process = OwnedProcess("bad", ["/does/not/exist"], self.artifacts)
        self.processes.append(process)
        with self.assertRaises(AssertionError):
            await process.start()
        await process.force_reap()
        self.assertIsNotNone(process.child.returncode)

    async def test_unexpected_clean_exit_cannot_become_a_scheduled_restart(self):
        process = await self.start("import time; print('ready',flush=True); time.sleep(.2)")
        await process.ready("ready")
        await asyncio.wait_for(process.child.wait(), 3)
        await process.stop()
        self.assertEqual(process.child.returncode, 125)


class SupervisorTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.root = short_root()
        self.run_id = str(uuid.uuid4())
        private_json(self.root / "owner.json", dict(run_id=self.run_id))
        args = SimpleNamespace(root=str(self.root), run_id=self.run_id, budget=40, lease=0.2)
        self.supervisor = Supervisor(args)
        self.supervisor.settings = dict(fd_soft=1024, fd_hard=4096)
        self.supervisor.initialized = True
        self.server_task = None

    async def asyncTearDown(self):
        await self.supervisor.stop("test cleanup")
        self.supervisor.done.set()
        if self.server_task:
            await asyncio.wait_for(self.server_task, 5)
        if self.root.exists():
            shutil.rmtree(self.root)

    async def start_child(self):
        child = await self.supervisor.start_process("fixture", [sys.executable, "-c",
            "import time; print('ready',flush=True); time.sleep(60)"])
        await child.ready("ready")
        return child

    async def test_heartbeat_expiry_retains_receipt_and_removes_secrets(self):
        child = await self.start_child()
        private_json(self.root / "host.toml", {"secret": "generated-only"})
        (self.root / "state").mkdir()
        self.server_task = asyncio.create_task(self.supervisor.run())
        async with asyncio.timeout(5):
            while self.supervisor.cleanup != "verified":
                await asyncio.sleep(0.05)
        self.assertEqual(self.supervisor.reason, "heartbeat expired")
        self.assertIsNotNone(child.child.returncode)
        self.assertFalse((self.root / "host.toml").exists())
        self.assertFalse((self.root / "state").exists())
        receipt = json.loads((self.root / "artifacts" / "cleanup.json").read_text())
        self.assertEqual(receipt["run_id"], self.run_id)
        self.assertEqual(receipt["cleanup"], "verified")

    async def test_hard_deadline_does_not_depend_on_heartbeat(self):
        child = await self.start_child()
        self.supervisor.lease = 100
        self.supervisor.deadline = time.monotonic() + 35.1
        self.server_task = asyncio.create_task(self.supervisor.run())
        async with asyncio.timeout(5):
            while self.supervisor.cleanup != "verified":
                await asyncio.sleep(0.05)
        self.assertEqual(self.supervisor.reason, "hard deadline")
        self.assertIsNotNone(child.child.returncode)

    async def test_wrong_runtime_identity_is_refused(self):
        with self.assertRaises(AssertionError):
            Supervisor(SimpleNamespace(root=str(self.root), run_id=str(uuid.uuid4()), budget=40, lease=1))

    async def test_only_owned_child_is_stopped(self):
        unrelated = await asyncio.create_subprocess_exec(sys.executable, "-c", "import time;time.sleep(60)")
        try:
            child = await self.start_child()
            await self.supervisor.stop()
            self.assertIsNotNone(child.child.returncode)
            self.assertIsNone(unrelated.returncode)
        finally:
            unrelated.terminate()
            await unrelated.wait()

    async def test_failed_report_cannot_skip_private_state_cleanup(self):
        child = await self.start_child()
        (self.root / "state").mkdir()
        private_json(self.root / "host.toml", {"secret": "private-state"})
        private_json(self.root / "relay.toml", {"token_env": "LOCHO_RELAY_TOKEN"})
        with patch.object(self.supervisor.artifacts, "json", side_effect=OSError("disk full")):
            receipt = await self.supervisor.stop()
        self.assertIn("sanitized cleanup report failed", receipt["errors"])
        self.assertFalse((self.root / "state").exists())
        self.assertFalse((self.root / "host.toml").exists())
        self.assertFalse((self.root / "relay.toml").exists())
        self.assertIsNotNone(child.child.returncode)

    async def test_custom_policy_wires_host_and_share_but_not_secret(self):
        self.supervisor.initialized = False
        (self.root / "locho").write_bytes(b"fake-binary-not-executed")
        policy = dict(url="https://relay.example.org:8443", token_env="LOCHO_RELAY_TOKEN",
                      include_n0_relays=False, quic_address_discovery=True)
        process = SimpleNamespace(ready=AsyncMock())
        outputs = [(0, b"locho 1.3.0"), (0, b"locho attach fake-host")]
        outputs += [(0, b"service:tcp:private-generated-secret") for _ in range(6)]
        with patch("remote_runner.environment", AsyncMock(return_value={"LOCHO_RELAY_TOKEN": "private-bearer"})), \
             patch.object(self.supervisor, "start_process", AsyncMock(return_value=process)) as start, \
             patch("remote_runner.capture", AsyncMock(side_effect=outputs)) as capture:
            await self.supervisor.initialize(dict(fd_soft=1024, fd_hard=4096, relay_env=None,
                                                  provenance="unknown", commit="unknown", dev_idle=2,
                                                  relay_policy=policy))
        self.assertIn("--relay-config", start.call_args_list[1].args[1])
        self.assertIn("--relay-config", capture.call_args_list[1].args)
        for call in capture.call_args_list[2:]:
            self.assertNotIn("--relay-config", call.args)
        self.assertNotIn("private-bearer", (self.root / "relay.toml").read_text())

    async def test_artifact_paths_cannot_read_private_state(self):
        await self.supervisor.stop()
        with self.assertRaises(AssertionError):
            await self.supervisor.dispatch("artifact", {"name": "../owner.json", "offset": 0})
        (self.supervisor.artifacts.root / "foreign").symlink_to(self.root / "owner.json")
        with self.assertRaises(AssertionError):
            await self.supervisor.dispatch("artifact", {"name": "foreign", "offset": 0})

    async def test_invalid_protocol_stops_owned_workload(self):
        child = await self.start_child()
        self.supervisor.lease = 100
        self.server_task = asyncio.create_task(self.supervisor.run())
        async with asyncio.timeout(3):
            while not (self.root / "c").exists():
                await asyncio.sleep(0.01)
        reader, writer = await asyncio.open_unix_connection(str(self.root / "c"))
        writer.write(encode(message("wrong", 1, "heartbeat")))
        await writer.drain()
        async with asyncio.timeout(5):
            while self.supervisor.cleanup != "verified":
                await asyncio.sleep(0.05)
        self.assertEqual(self.supervisor.reason, "invalid protocol")
        self.assertIsNotNone(child.child.returncode)
        writer.close()
        await writer.wait_closed()


class FixtureObservationTests(unittest.IsolatedAsyncioTestCase):
    async def test_persistent_completed_large_frame_stops_sampling_before_small_frames(self):
        root = short_root()
        observer = FixtureObservation(Artifacts(root / "artifacts"))
        fixture = Fixture(root / "observed", observer.callback("unix"))
        writer = None
        try:
            await fixture.start()
            reader, writer = await connect(fixture.address)
            identity = uuid.uuid4().bytes
            await exchange(reader, writer, identity, 1, 1048576)
            self.assertEqual(observer.active, {})
            before = observer.artifacts.events["fixture-progress.jsonl"]
            for sequence in range(2, 10):
                await exchange(reader, writer, identity, sequence, 64)
                observer.sample()
            self.assertEqual(observer.artifacts.events["fixture-progress.jsonl"], before)
            self.assertEqual(observer.active, {})
            self.assertEqual(fixture.errors, 0)
            await exchange(reader, writer, identity, 10, 1048576)
            self.assertEqual(observer.active, {})
            self.assertGreater(observer.artifacts.events["fixture-progress.jsonl"], before)
        finally:
            if writer:
                await close(writer)
            await fixture.stop()
            shutil.rmtree(root)

    async def test_request_read_and_response_drain_are_distinct_from_validation(self):
        root = short_root()
        artifacts = Artifacts(root / "artifacts")
        observer = FixtureObservation(artifacts)
        fixture = Fixture(root / "not-bound", observer.callback("unix"))
        released = asyncio.Event()
        class Writer:
            def __init__(self):
                self.queued = 0
                self.transport = SimpleNamespace(get_write_buffer_size=lambda: self.queued)
            def write(self, data):
                self.queued += len(data)
            async def drain(self):
                await released.wait()
                self.queued = 0
            def close(self):
                pass
            async def wait_closed(self):
                pass
        identity = uuid.uuid4().bytes
        reader = asyncio.StreamReader()
        reader.feed_data(HEADER.pack(b"LUT1", 2, identity, 1, 1048576) + payload(1048576, identity, 1))
        reader.feed_eof()
        task = asyncio.create_task(fixture.handle(reader, Writer()))
        try:
            async with asyncio.timeout(3):
                while not observer.active or not next(iter(observer.active.values()))["request_read_complete"]:
                    await asyncio.sleep(0.001)
            observer.sample()
            self.assertEqual(fixture.highwater, {})
            observed = [json.loads(line) for line in (artifacts.root / "fixture-progress.jsonl").read_text().splitlines()]
            active = observed[-1]
            self.assertTrue(active["request_read_complete"])
            self.assertFalse(active["response_drained"])
            self.assertFalse(active["request_validated"])
            self.assertNotIn("reader", active)
            self.assertNotIn("writer", active)
            released.set()
            await asyncio.wait_for(task, 3)
            self.assertEqual(fixture.highwater[identity], 1)
            self.assertEqual(fixture.errors, 0)
            self.assertEqual(observer.active, {})
        finally:
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
            shutil.rmtree(root)


class DetachedBridgeTests(unittest.IsolatedAsyncioTestCase):
    """Real local pipes/processes; fake Locho CLI, no SSH or Locho transport."""

    async def make_bridge(self, lease="1"):
        root = short_root()
        run_id = str(uuid.uuid4())
        private_json(root / "owner.json", {"run_id": run_id})
        for name in ("remote_runner.py", "native_support.py", "unix_streams.py"):
            shutil.copyfile(Path(__file__).parent / name, root / name)
        (root / "locho").write_text(
            f"#!{sys.executable}\n"
            "import signal,sys,time\n"
            "command=sys.argv[1]\n"
            "if command=='--version': print('locho 1.3.0')\n"
            "elif command=='share': print('locho attach fake-host')\n"
            "elif command=='secret': print(sys.argv[2]+':tcp:'+'a'*32)\n"
            "elif command=='host':\n"
            " signal.signal(signal.SIGTERM,lambda *_:sys.exit(0))\n"
            " print('Services:',flush=True)\n"
            " time.sleep(60)\n")
        (root / "locho").chmod(0o700)
        bridge = await asyncio.create_subprocess_exec(
            sys.executable, str(root / "remote_runner.py"), "--root", str(root), "--run-id", run_id,
            "--budget", "40", "--lease", lease, stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        return root, run_id, bridge

    async def test_graceful_finish_reaps_watchdog_before_bridge_exit(self):
        root, run_id, bridge = await self.make_bridge(lease="5")
        try:
            for identity, command, payload in (
                    (1, "initialize", dict(fd_soft=1024, fd_hard=4096, relay_env=None,
                                           provenance="unknown", commit="unknown", dev_idle=2)),
                    (2, "stop", {}), (3, "finish", {})):
                bridge.stdin.write(encode(message(run_id, identity, command, payload)))
                await bridge.stdin.drain()
                response = await asyncio.wait_for(receive(bridge.stdout), 15)
                self.assertTrue(response["ok"])
            # Keep stdin open: finish must disconnect the bridge's Unix client
            # itself, rather than depend on EOF from its controller/SSH parent.
            await asyncio.wait_for(bridge.wait(), 5)
            self.assertEqual(bridge.returncode, 0)
            self.assertFalse(root.exists())
        finally:
            if bridge.returncode is None:
                bridge.kill()
                await bridge.wait()
            if (root / "c").exists():
                reader, writer = await asyncio.open_unix_connection(str(root / "c"))
                try:
                    for identity, command in ((4, "stop"), (5, "finish")):
                        writer.write(encode(message(run_id, identity, command)))
                        await writer.drain()
                        await asyncio.wait_for(receive(reader), 5)
                finally:
                    writer.close()
                    await writer.wait_closed()
                async with asyncio.timeout(5):
                    while root.exists():
                        await asyncio.sleep(0.05)
            elif root.exists():
                shutil.rmtree(root)

    async def test_bridge_loss_leaves_independent_watchdog_to_reap_owned_children(self):
        root, run_id, bridge = await self.make_bridge()
        try:
            bridge.stdin.write(encode(message(run_id, 1, "initialize", dict(fd_soft=1024, fd_hard=4096,
                                     relay_env=None, provenance="unknown", commit="unknown", dev_idle=2))))
            await bridge.stdin.drain()
            response = await asyncio.wait_for(receive(bridge.stdout), 15)
            self.assertTrue(response["ok"])
            endpoint = response["payload"]["metadata"]["fixtures"]["tt"][1]
            identity = uuid.uuid4().bytes
            reader, writer = await connect(endpoint)
            await exchange(reader, writer, identity, 1, 16, half_close=True)
            await close(writer)
            bridge.stdin.write(encode(message(run_id, 2, "restart", {"process": "fixtures"})))
            await bridge.stdin.drain()
            restarted = await asyncio.wait_for(receive(bridge.stdout), 10)
            self.assertTrue(restarted["ok"])
            self.assertEqual(restarted["payload"]["generation"], 2)
            reader, writer = await connect(endpoint)
            await exchange(reader, writer, identity, 2, 16, half_close=True)
            await close(writer)
            bridge.kill()  # simulate SSH bridge loss, not a Locho fault
            await bridge.wait()
            receipt = root / "artifacts" / "cleanup.json"
            async with asyncio.timeout(10):
                while not receipt.exists():
                    await asyncio.sleep(0.05)
            result = json.loads(receipt.read_text())
            self.assertEqual(result["cleanup"], "verified")
            self.assertEqual(result["reason"], "heartbeat expired")
            self.assertFalse((root / "state").exists())
            for process in result["processes"].values():
                with self.assertRaises(AssertionError):
                    await process_identity(process["pid"])
        finally:
            if bridge.returncode is None:
                bridge.kill()
                await bridge.wait()
            if (root / "c").exists():
                reader, writer = await asyncio.open_unix_connection(str(root / "c"))
                try:
                    for identity, command in ((3, "stop"), (4, "finish")):
                        writer.write(encode(message(run_id, identity, command)))
                        await writer.drain()
                        await asyncio.wait_for(receive(reader), 5)
                finally:
                    writer.close()
                    await writer.wait_closed()
                async with asyncio.timeout(5):
                    while root.exists():
                        await asyncio.sleep(0.05)
            elif root.exists():
                shutil.rmtree(root)


if __name__ == "__main__":
    unittest.main()
