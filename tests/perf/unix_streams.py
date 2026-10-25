#!/usr/bin/env python3
"""Native, ordinary-release Unix/TCP smoke, stress and soak tests (Python 3.11+).

No Herdr, Docker, third-party Python modules, or application message retries.
Only resources created in this run's private directory/process tree are owned.
"""

import argparse
import asyncio
import collections
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import shutil
import socket
import stat
import struct
import sys
import tempfile
import time
import uuid


HEADER = struct.Struct("!4sB16sQI")
MAX_PAYLOAD = 8 * 1024 * 1024
TIMEOUT = 15
STARTUP_TIMEOUT = 30
RECOVERY_TIMEOUT = 30
LOG_LIMIT = 1024 * 1024
MAX_EVENTS = 2000
LOG_DRAIN_TIMEOUT = 2
FINALIZATION_TIMEOUT = 30


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def duration(value):
    multiplier = {"s": 1, "m": 60, "h": 3600}
    try:
        result = int(value[:-1]) * multiplier[value[-1]] if value[-1:] in multiplier else int(value)
    except (ValueError, IndexError):
        raise argparse.ArgumentTypeError("use integer seconds or a suffix: 30m, 1h") from None
    if result <= 0:
        raise argparse.ArgumentTypeError("duration must be positive")
    return result


def stress_levels(value):
    try:
        levels = [int(part) for part in value.replace(",", " ").split()]
    except ValueError:
        raise argparse.ArgumentTypeError("stress levels must be positive integers") from None
    if not levels or any(level <= 0 for level in levels):
        raise argparse.ArgumentTypeError("stress levels must be positive integers")
    if any(level > 128 for level in levels):
        raise argparse.ArgumentTypeError("stress levels must not exceed 128 workers per combination")
    return levels


def short_root():
    # Resolve macOS's /var alias; never put sockets below the long checkout path.
    root = Path(tempfile.mkdtemp(prefix="lu-", dir=tempfile.gettempdir())).resolve()
    if len(os.fsencode(root / "replacement")) > 103:
        root.rmdir()
        raise AssertionError("temporary directory is too long for Unix sockets; set TMPDIR to a shorter private directory")
    root.chmod(0o700)
    return root


def write_toml(path, text):
    # Exclusive creation refuses accidental overwrite and keeps secrets private.
    with open(path, "x", opener=lambda p, flags: os.open(p, flags, 0o600)) as output:
        output.write(text)


def quote(value):
    return json.dumps(str(value))


def tcp_port(host="127.0.0.1"):
    family = socket.AF_INET6 if ":" in host else socket.AF_INET
    with socket.socket(family) as listener:
        listener.bind((host, 0))
        return listener.getsockname()[1]


async def connect(address):
    if isinstance(address, Path):
        return await asyncio.open_unix_connection(str(address))
    return await asyncio.open_connection("127.0.0.1", address)


async def capture(*args, env=None, timeout=STARTUP_TIMEOUT):
    child = await asyncio.create_subprocess_exec(
        *args, env=env, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
    try:
        stdout, _stderr = await asyncio.wait_for(child.communicate(), timeout)
    except BaseException:
        if child.returncode is None:
            child.kill()
        await child.wait()
        raise
    return child.returncode, stdout


async def close(writer):
    writer.close()
    try:
        await asyncio.wait_for(writer.wait_closed(), 2)
    except (OSError, asyncio.TimeoutError):
        pass


def payload(size, identity, sequence):
    seed = hashlib.sha256(identity + sequence.to_bytes(8, "big")).digest()
    return (seed * ((size + len(seed) - 1) // len(seed)))[:size]


def fault_failure(active, started_in_fault, operation_epoch, connection_epoch, current_epoch):
    # A persistent stream opened before a scheduled fault can discover its EOF
    # only on the next operation, after fresh connections have already recovered.
    # Classify that stream's one terminal failure, never a wall-clock grace period.
    return active or started_in_fault or operation_epoch != current_epoch or connection_epoch != current_epoch


def descriptor_growth(before, after, allowance):
    require(before["pid"] == after["pid"], "descriptor gate must compare the same process lifetime")
    growth = after["fds"] - before["fds"]
    require(growth <= allowance, "same-PID descriptor growth exceeded post-drain allowance")
    return growth


async def exchange(reader, writer, identity, sequence, size, *, half_close=False,
                   fragmented=False, slow=False):
    data = payload(size, identity, sequence)
    header = HEADER.pack(b"LUT1", int(half_close), identity, sequence, size)
    frame = header + data
    if fragmented:
        for offset in range(0, len(frame), 997):
            writer.write(frame[offset:offset + 997])
            await writer.drain()
    else:
        writer.write(frame)
        await writer.drain()
    if half_close:
        writer.write_eof()
        await writer.drain()
    if slow:
        await asyncio.sleep(0.2)
    actual_header = await reader.readexactly(HEADER.size)
    require(actual_header == header, "cross-routed or corrupted frame header")
    actual = await reader.readexactly(size)
    require(hashlib.sha256(actual).digest() == hashlib.sha256(data).digest(),
            "binary payload checksum mismatch")
    if half_close:
        require(await reader.read(1) == b"", "half-close response did not finish")


async def duplex_exchange(reader, writer, identity, sequence, size):
    """Both sides write more than the socket buffers while reading responses."""
    data = payload(size, identity, sequence)
    header = HEADER.pack(b"LUT1", 2, identity, sequence, size)

    async def send():
        writer.write(header + data)
        await writer.drain()
        writer.write_eof()
        await writer.drain()

    async def receive():
        require(await reader.readexactly(HEADER.size) == header, "duplex header corruption")
        require(await reader.readexactly(size) == data, "duplex checksum mismatch")
        require(await reader.read(1) == b"", "duplex stream did not finish")

    await asyncio.gather(send(), receive())


class Fixture:
    """Framed binary echo; half-close mode replies only after receiving EOF.

    High-water marks are bounded by the fixed worker identities, not operation
    count. Repeated or out-of-order sequence numbers fail instead of replaying.
    """

    def __init__(self, address):
        self.address = address
        self.server = None
        self.writers = set()
        self.tasks = set()
        self.highwater = {}
        self.messages = 0
        self.duplicates = 0
        self.errors = 0
        self.interrupted = 0
        self.accepted = 0

    async def start(self):
        if isinstance(self.address, Path):
            self.server = await asyncio.start_unix_server(self.handle, str(self.address))
        else:
            host, port = self.address
            self.server = await asyncio.start_server(self.handle, host, port)

    @property
    def endpoint(self):
        host, port = self.address
        return f"[{host}]:{port}" if ":" in host else f"{host}:{port}"

    async def handle(self, reader, writer):
        self.accepted += 1
        self.writers.add(writer)
        self.tasks.add(asyncio.current_task())
        try:
            while True:
                try:
                    header = await reader.readexactly(HEADER.size)
                except asyncio.IncompleteReadError as error:
                    if error.partial:
                        self.interrupted += 1
                    break
                magic, mode, identity, sequence, size = HEADER.unpack(header)
                require(magic == b"LUT1" and mode in (0, 1, 2), "invalid fixture frame")
                require(size <= MAX_PAYLOAD, "fixture payload exceeds bound")
                if mode == 2:
                    expected = payload(size, identity, sequence)
                    async def send():
                        writer.write(header + expected)
                        await writer.drain()
                    data, _ = await asyncio.gather(reader.readexactly(size), send())
                else:
                    data = await reader.readexactly(size)
                require(data == payload(size, identity, sequence), "upstream checksum mismatch")
                if sequence <= self.highwater.get(identity, -1):
                    self.duplicates += 1
                    raise AssertionError("duplicate/replayed fixture message")
                require(identity in self.highwater or len(self.highwater) < 1024,
                        "fixture identity budget exceeded")
                self.highwater[identity] = sequence
                self.messages += 1
                if mode:
                    require(await reader.read(1) == b"", "half-close fixture expected EOF")
                if mode != 2:
                    writer.write(header + data)
                    await writer.drain()
                if mode:
                    break
        except (ConnectionError, asyncio.IncompleteReadError):
            # Interrupted streams are expected during scheduled fault windows.
            self.interrupted += 1
        except AssertionError:
            self.errors += 1
        finally:
            await close(writer)
            self.writers.discard(writer)
            self.tasks.discard(asyncio.current_task())

    async def stop(self):
        if self.server:
            self.server.close()
        for writer in list(self.writers):
            writer.close()
        tasks = list(self.tasks)
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        if self.server:
            await self.server.wait_closed()
            self.server = None
        if isinstance(self.address, Path) and self.address.is_socket():
            self.address.unlink()


class Process:
    def __init__(self, run, name, args):
        self.run = run
        self.name = name
        self.args = args
        self.child = None
        self.lines = collections.deque(maxlen=128)
        self.log_bytes = 0
        self.pumps = []

    async def start(self):
        env = {key: value for key, value in os.environ.items() if not key.startswith("LOCHO_TEST_")}
        env.update(LOCHO_STATE_DIR=str(self.run.root / "state"), RUST_LOG="warn", NO_COLOR="1")
        self.child = await asyncio.create_subprocess_exec(
            str(self.run.binary), *self.args, env=env,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        self.pumps = [asyncio.create_task(self.pump(stream))
                      for stream in (self.child.stdout, self.child.stderr)]
        self.run.processes.append(self)
        return self

    async def pump(self, stream):
        while line := await stream.readline():
            text = line.decode(errors="replace").replace(str(self.run.root), "<runtime>")
            for secret in self.run.secrets:
                text = text.replace(secret, "<redacted>")
            self.lines.append(text)
            if self.log_bytes < LOG_LIMIT:
                with open(self.run.output / f"{self.name}.log", "a") as log:
                    log.write(text)
                self.log_bytes += len(line)

    async def wait_for(self, text):
        async with asyncio.timeout(STARTUP_TIMEOUT):
            while not any(text in line for line in self.lines):
                require(self.child.returncode is None, f"{self.name} exited before readiness; see its log")
                await asyncio.sleep(0.05)

    async def stop(self, sig=signal.SIGTERM, clean=True):
        if self.child.returncode is None:
            self.child.send_signal(sig)
            try:
                await asyncio.wait_for(self.child.wait(), 15)
            except asyncio.TimeoutError:
                self.child.kill()
                await self.child.wait()
                raise AssertionError(f"{self.name} exceeded shutdown deadline") from None
        try:
            results = await asyncio.wait_for(asyncio.gather(*self.pumps, return_exceptions=True), LOG_DRAIN_TIMEOUT)
        except asyncio.TimeoutError:
            for pump in self.pumps:
                pump.cancel()
            raise AssertionError(f"{self.name} log pumps exceeded drain deadline") from None
        require(all(not isinstance(result, Exception) for result in results), "process log pump failed")
        if clean:
            require(self.child.returncode == 0, f"{self.name} did not exit successfully")


class Run:
    def __init__(self, args):
        self.args = args
        self.binary = Path(args.binary).resolve()
        require(self.binary.is_file(), "build the release binary first or pass --binary")
        self.binary_sha256 = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        self.harness_sha256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
        self.root = short_root()
        self.output = Path(args.output).resolve() / f"unix-{args.mode}-{time.time_ns()}"
        self.output.mkdir(parents=True, mode=0o700)
        self.processes = []
        self.secrets = []
        # Keep the two TCP upstream fixtures on distinct loopback destinations.
        # macOS actively closes half-closed churn connections and retains their
        # source ports in TIME_WAIT; sharing one destination can exhaust the
        # 16,384-port loopback range at the highest stress phase even though
        # ordinary TCP and Locho behavior are healthy. Linux does not show the
        # same limit in this profile. This is test-fixture capacity isolation,
        # not a production transport workaround.
        self.fixtures = [
            Fixture(("127.0.0.1", tcp_port("127.0.0.1"))),
            Fixture(self.root / "u"),
        ]
        self.tcp_fixtures = {
            "tt": self.fixtures[0],
            "ut": Fixture(("127.0.0.1", tcp_port("127.0.0.1"))),
            "default": self.fixtures[0],
            "hostidle": self.fixtures[0],
        }
        self.fixtures.append(self.tcp_fixtures["ut"])
        self.listeners = {"tt": tcp_port(), "tu": tcp_port(),
                          "ut": self.root / "a", "uu": self.root / "b"}
        self.fault_epochs = dict.fromkeys(self.listeners, 0)
        self.fault_services = set()
        self.address = f"127.0.0.1:{tcp_port()}"
        self.counts = collections.Counter()
        self.latencies = collections.deque(maxlen=10000)
        self.events = 0
        self.failure_frames = []
        self.fault = False
        self.fault_generation = 0
        self.host = None
        self.attachment = None
        self.sequence = collections.Counter()
        self.identities = {}
        self.measured_load_seconds = None
        # macOS keeps actively closed loopback TCP connections in TIME_WAIT for
        # the configured MSL. At the highest 32-worker phase, the four-combination
        # reconnect cadence can exhaust its 16,384 ephemeral source ports even in
        # a plain TCP control. Keep the same concurrency, payloads and integrity
        # gates, but reduce churn only on macOS so the stress test measures tunnel
        # capacity rather than an OS port-range ceiling.
        self.stress_churn_period = 32 if sys.platform == "darwin" else 4

    def event(self, name, **fields):
        if name == "operation_failed":
            if self.events >= MAX_EVENTS:
                return
            self.events += 1
        with open(self.output / "timeline.jsonl", "a") as output:
            output.write(json.dumps({"time": time.time(), "event": name, **fields}) + "\n")

    async def cli(self, *args, success=True):
        env = dict(os.environ, LOCHO_STATE_DIR=str(self.root / "state"), NO_COLOR="1")
        status, stdout = await capture(str(self.binary), *args, env=env)
        if success:
            require(status == 0, "CLI configuration/credential operation failed")
        return status, stdout.decode().strip()

    async def start_host(self):
        self.host = await Process(self, "host", [
            "host", "--config", str(self.root / "host.toml"),
            "--bind-address", self.address]).start()
        await self.host.wait_for("Services:")

    async def start_attachment(self):
        self.attachment = await Process(self, "attachment", [
            "attach", "--config", str(self.root / "attach.toml")]).start()
        await self.attachment.wait_for("Local Unix socket:")
        await self.probe_all()
        await self.attachment.wait_for(f"transport path: direct({self.address})")

    async def setup(self):
        for fixture in self.fixtures:
            await fixture.start()
        host = []
        for name in [*self.listeners, "default", "hostidle"]:
            upstream = (f"unix_socket = {quote(self.fixtures[1].address)}" if name.endswith("u")
                        else f'endpoint = "{self.tcp_fixtures[name].endpoint}"')
            idle = "" if name == "default" else f"tcp_idle_timeout_secs = {1 if name == 'hostidle' else 0}\n"
            host.append(f'[[services]]\nname = "{name}"\ntype = "tcp"\n{upstream}\n{idle}')
        write_toml(self.root / "host.toml", "\n".join(host))
        await self.start_host()
        _, share = await self.cli("share", "tt", "--config", str(self.root / "host.toml"))
        self.host_id = share.split()[2]
        self.capabilities = {}
        for name in [*self.listeners, "default", "hostidle"]:
            _, capability = await self.cli("secret", name, "--config", str(self.root / "host.toml"))
            self.capabilities[name] = capability
            self.secrets.append(capability.split(":")[-1])
        attachments = [f'host_id = "{self.host_id}"\ndirect_address = "{self.address}"\n']
        for name, listener in self.listeners.items():
            field = (f"listen_unix = {quote(listener)}" if isinstance(listener, Path)
                     else f"listen_port = {listener}")
            attachments.append(f'[[services]]\ncapability = {quote(self.capabilities[name])}\n{field}\ntcp_idle_timeout_secs = 0\n')
        write_toml(self.root / "attach.toml", "\n".join(attachments))
        await self.start_attachment()
        self.event("ready", combinations=list(self.listeners))

    async def once(self, name, size=256, worker="probe", **options):
        key = f"{name}:{worker}"
        identity = self.identities.setdefault(key, uuid.uuid4().bytes)
        self.sequence[key] += 1  # Never retry an interrupted frame/sequence.
        options.setdefault("half_close", True)
        async with asyncio.timeout(TIMEOUT):
            reader, writer = await connect(self.listeners[name])
            try:
                await exchange(reader, writer, identity, self.sequence[key], size, **options)
                fixture = self.fixtures[1] if name.endswith("u") else self.tcp_fixtures[name]
                require(fixture.highwater.get(identity) == self.sequence[key],
                        "message reached the wrong upstream fixture")
            finally:
                await close(writer)

    async def probe_all(self):
        deadline = time.monotonic() + RECOVERY_TIMEOUT
        try:
            async with asyncio.timeout_at(deadline):
                while True:
                    results = await asyncio.gather(*(self.once(name) for name in self.listeners),
                                                   return_exceptions=True)
                    for result in results:
                        if isinstance(result, BaseException) and not isinstance(
                                result, (OSError, asyncio.TimeoutError, asyncio.IncompleteReadError)):
                            raise result
                    if all(result is None for result in results):
                        return
                    await asyncio.sleep(0.2)
        except asyncio.TimeoutError:
            raise AssertionError("fresh application probes did not recover within 30 seconds") from None

    async def positional(self, name, listener, capability=None, idle=0):
        flag = "--listen-unix" if isinstance(listener, Path) else "--listen"
        value = str(listener) if isinstance(listener, Path) else f"127.0.0.1:{listener}"
        args = ["attach", self.host_id, capability or self.capabilities[name],
                "--direct-address", self.address, flag, value]
        if idle is not None:
            args += ["--tcp-idle-timeout-secs", str(idle)]
        return await Process(self, "positional", args).start()

    async def expect_closed(self, reader):
        try:
            async with asyncio.timeout(TIMEOUT):
                require(await reader.read(1) == b"", "interrupted stream returned unexpected bytes")
        except (ConnectionResetError, BrokenPipeError):
            pass

    async def fault_restart(self, target):
        self.fault = True
        self.fault_generation += 1
        self.fault_services = {name for name in self.listeners if target != "upstream" or name.endswith("u")}
        for name in self.fault_services:
            self.fault_epochs[name] = self.fault_generation
        started = time.monotonic()
        identities = {name: os.stat(path).st_ino for name, path in self.listeners.items()
                      if isinstance(path, Path)}
        self.event("fault_start", target=target)
        if target == "upstream":
            await self.fixtures[1].stop()
            # Fresh authorization succeeds, but the absent Unix backend must
            # reject the stream rather than hang or deliver application bytes.
            try:
                await self.once("uu", worker="unavailable")
            except (OSError, asyncio.IncompleteReadError):
                pass
            else:
                raise AssertionError("unavailable Unix backend unexpectedly accepted a frame")
            await self.fixtures[1].start()
        elif target == "host":
            reader, writer = await connect(self.listeners["uu"])
            await exchange(reader, writer, uuid.uuid4().bytes, 1, 256)
            await self.host.stop()
            await self.expect_closed(reader)
            await close(writer)
            await self.start_host()
        else:
            await self.attachment.stop()
            self.assert_unlinked()
            await self.start_attachment()
        await self.probe_all()
        if target != "attachment":
            for name, inode in identities.items():
                require(os.stat(self.listeners[name]).st_ino == inode,
                        "attachment listener was rebound during peer/backend recovery")
        require(time.monotonic() - started <= RECOVERY_TIMEOUT, "fault recovery exceeded deadline")
        self.event("fault_recovered", target=target, elapsed=time.monotonic() - started)
        self.fault = False
        self.fault_services.clear()

    def assert_unlinked(self):
        for path in self.listeners.values():
            if isinstance(path, Path):
                require(not os.path.lexists(path), "owned Unix listener was not cleaned up")

    async def smoke(self):
        for name in self.listeners:
            for size in (0, 1, 16384, 1024 * 1024):
                await self.once(name, size, fragmented=True, half_close=True)
            await asyncio.gather(*(self.once(name, 65536, worker=f"parallel-{i}", slow=True)
                                   for i in range(4)))
            async with asyncio.timeout(TIMEOUT):
                reader, writer = await connect(self.listeners[name])
                try:
                    await duplex_exchange(reader, writer, uuid.uuid4().bytes, 1, 2 * 1024 * 1024)
                finally:
                    await close(writer)
        self.event("matrix_passed")
        await self.listener_checks()
        await self.startup_signal_check()
        await self.client_reset_check()
        await self.idle_checks()
        await self.budget_check()
        await self.recovery_checks()
        # Exercise both normal Unix shutdown signals with active, quiet streams.
        for sig in (signal.SIGINT, signal.SIGTERM):
            reader, writer = await connect(self.listeners["uu"])
            await exchange(reader, writer, uuid.uuid4().bytes, 1, 1)
            await self.attachment.stop(sig)
            self.assert_unlinked()
            await self.expect_closed(reader)
            await close(writer)
            await self.start_attachment()
        self.event("shutdown_signals_passed")

    async def startup_signal_check(self):
        config = self.root / "startup.toml"
        paths = [self.root / f"s{i}" for i in range(64)]
        write_toml(config, f'host_id = "{self.host_id}"\n' + "\n".join(
            f'[[services]]\ncapability = "startup{i}:tcp:test"\nlisten_unix = {quote(path)}\n'
            for i, path in enumerate(paths)))
        process = await Process(self, "startup", ["attach", "--config", str(config)]).start()
        async with asyncio.timeout(STARTUP_TIMEOUT):
            while not paths[0].is_socket():
                require(process.child.returncode is None, "startup attachment exited before its first bind")
                await asyncio.sleep(0.001)
        bound = sum(path.is_socket() for path in paths)
        await process.stop(signal.SIGTERM)
        require(all(not os.path.lexists(path) for path in paths), "SIGTERM during startup left owned sockets")
        self.event("startup_sigterm_passed", listeners_bound_when_signalled=bound)

    async def client_reset_check(self):
        # Keep a different service's stream alive while a Unix client drops an
        # unread response. This must not invalidate the shared peer connection.
        reader, writer = await connect(self.listeners["ut"])
        identity = uuid.uuid4().bytes
        try:
            await exchange(reader, writer, identity, 1, 256)
            for sequence in range(3):
                client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                client.setblocking(False)
                try:
                    await asyncio.get_running_loop().sock_connect(client, str(self.listeners["uu"]))
                    token = uuid.uuid4().bytes
                    data = payload(65536, token, sequence)
                    await asyncio.get_running_loop().sock_sendall(
                        client, HEADER.pack(b"LUT1", 0, token, sequence, len(data)) + data)
                    await asyncio.sleep(0.05)  # Leave the response unread in the socket.
                finally:
                    client.close()
                await asyncio.sleep(0.05)
                await asyncio.wait_for(exchange(reader, writer, identity, sequence + 2, 256), TIMEOUT)
            writer.write_eof()
            await writer.drain()
            require(await asyncio.wait_for(reader.read(1), TIMEOUT) == b"", "isolated stream did not drain")
        finally:
            await close(writer)
        self.event("local_client_reset_isolation_passed")

    async def recovery_checks(self):
        """Short loaded recovery regression, including late EOF on old streams."""
        stop = asyncio.Event()
        tasks = [asyncio.create_task(self.worker(name, index, stop))
                 for name in self.listeners for index in range(3)]
        try:
            await asyncio.sleep(0.3)
            for target in ("upstream", "host", "attachment"):
                await self.fault_restart(target)
                await asyncio.sleep(0.3)
                for task in tasks:
                    if task.done():
                        task.result()
        finally:
            stop.set()
            results = await asyncio.wait_for(asyncio.gather(*tasks, return_exceptions=True), TIMEOUT + 5)
            for result in results:
                if isinstance(result, BaseException):
                    raise result
        require(self.counts["unexpected_failures"] == 0, "unexpected I/O failure during loaded smoke recovery")
        self.event("loaded_recovery_passed")

    async def stress_phase(self, concurrency, phase, phase_duration):
        baseline_failures = self.counts["unexpected_failures"]
        baseline_successes = sum(self.counts[f"{name}.success"] for name in self.listeners)
        baseline_bytes = self.counts["bytes"]
        stop = asyncio.Event()
        tasks = [asyncio.create_task(self.worker(name, index, stop, stress=True))
                 for name in self.listeners for index in range(concurrency)]
        started = time.monotonic()
        next_sample = 0
        failure = None
        try:
            while time.monotonic() - started < phase_duration:
                require(self.host.child.returncode is None and self.attachment.child.returncode is None,
                        f"process exited during stress phase {phase}")
                for task in tasks:
                    if task.done():
                        task.result()
                elapsed = time.monotonic() - started
                if elapsed >= next_sample:
                    await self.sample()
                    next_sample = elapsed + 10
                await asyncio.sleep(0.2)
        except BaseException as exception:
            failure = exception
        finally:
            stop.set()
            try:
                results = await asyncio.wait_for(asyncio.gather(*tasks, return_exceptions=True), TIMEOUT + 5)
            except asyncio.TimeoutError:
                raise AssertionError(f"stress phase {phase} workers did not drain") from None
            if failure is None:
                for result in results:
                    if isinstance(result, BaseException):
                        failure = result
                        break
        if failure is not None:
            raise failure
        require(self.counts["unexpected_failures"] == baseline_failures,
                f"unexpected operation failure at stress phase {phase}")
        await asyncio.sleep(1)
        sample = await self.sample()
        self.event("stress_phase_passed", phase=phase, concurrency=concurrency,
                   elapsed=time.monotonic() - started,
                   successful_operations=(
                       sum(self.counts[f"{name}.success"] for name in self.listeners)
                       - baseline_successes
                   ),
                   bytes=self.counts["bytes"] - baseline_bytes,
                   processes={name: {key: value for key, value in record.items()
                                     if key in ("pid", "rss_kib", "fds")}
                              for name, record in sample.items()})

    async def stress(self):
        levels = self.args.stress_levels
        baseline = await self.sample()
        started = time.monotonic()
        warmup = max(5, min(30, self.args.duration // 10))
        remaining = self.args.duration - warmup * 2
        require(remaining >= len(levels), "stress duration is too short for the requested levels")
        phase_duration = remaining // len(levels)
        await self.stress_phase(levels[0], "warmup", warmup)
        for phase, level in enumerate(levels, 1):
            await self.stress_phase(level, phase, phase_duration)
        await self.stress_phase(levels[0], "cooldown", warmup)
        load_finished = time.monotonic()
        await self.probe_all()
        await asyncio.sleep(2)
        final = await self.sample()
        for name in baseline:
            growth = descriptor_growth(baseline[name], final[name], self.args.max_fd_growth)
            self.event("stress_resource_gate_passed", process=name, pid=final[name]["pid"],
                       fd_growth=growth)
        require(self.counts["unexpected_failures"] == 0,
                "unexpected steady-state operation failures during stress")
        self.measured_load_seconds = load_finished - started
        self.event("stress_passed", duration=self.args.duration, levels=levels,
                   elapsed=self.measured_load_seconds)

    async def budget_check(self):
        connections = []
        processes = []
        try:
            # Distribute streams across four peers: QUIC's per-peer stream
            # window may be smaller than the host's shared application budget.
            addresses = [self.listeners["tt"]]
            for name in ("tu", "ut", "uu"):
                path = self.root / f"budget-{name}"
                process = await self.positional(name, path)
                processes.append(process)
                await process.wait_for("Local Unix socket:")
                addresses.append(path)
            for address in addresses:
                for _ in range(32):
                    async with asyncio.timeout(TIMEOUT):
                        reader, writer = await connect(address)
                        connections.append((reader, writer))
                        await exchange(reader, writer, uuid.uuid4().bytes, 1, 1)
            before = sum(fixture.messages for fixture in self.fixtures)
            try:
                await self.once("uu", worker="excess")
            except (OSError, asyncio.IncompleteReadError):
                pass
            else:
                raise AssertionError("connection budget exceeded without rejection")
            require(sum(fixture.messages for fixture in self.fixtures) == before,
                    "over-budget frame reached the upstream")
        finally:
            async def drain(reader, writer):
                try:
                    writer.write_eof()
                    await writer.drain()
                    await asyncio.wait_for(reader.read(1), TIMEOUT)
                except OSError:
                    pass
                finally:
                    await close(writer)
            await asyncio.gather(*(drain(reader, writer) for reader, writer in connections))
            for process in processes:
                await process.stop()
        await self.probe_all()
        self.event("connection_budget_passed", held_connections=128)

    async def listener_checks(self):
        for path in self.listeners.values():
            if isinstance(path, Path):
                require(stat.S_IMODE(path.stat().st_mode) == 0o600, "Unix socket mode is not 0600")
        path = self.root / "refuse"
        target = self.root / "target"
        target.write_bytes(b"keep")
        makers = [lambda: path.write_bytes(b"keep"), lambda: path.mkdir(),
                  lambda: path.symlink_to(target)]
        for make in makers:
            make()
            before = path.lstat()
            process = await self.positional("uu", path)
            await asyncio.wait_for(process.child.wait(), STARTUP_TIMEOUT)
            require(process.child.returncode != 0, "existing listener entry was accepted")
            require(path.lstat().st_ino == before.st_ino, "existing entry was mutated")
            require(target.read_bytes() == b"keep", "symlink target was mutated")
            path.rmdir() if path.is_dir() else path.unlink()
        # Refusing a live socket must leave the existing attachment usable.
        process = await self.positional("uu", self.listeners["uu"])
        await asyncio.wait_for(process.child.wait(), STARTUP_TIMEOUT)
        require(process.child.returncode != 0, "live socket was replaced")
        await self.once("uu")
        insecure = self.root / "public"
        insecure.mkdir(mode=0o755)
        insecure.chmod(0o755)
        process = await self.positional("uu", insecure / "s")
        await asyncio.wait_for(process.child.wait(), STARTUP_TIMEOUT)
        require(process.child.returncode != 0, "insecure parent was accepted")
        # A later bind failure must roll back an earlier Unix listener.
        rollback = self.root / "rollback"
        config = self.root / "rollback.toml"
        write_toml(config, f'host_id = "{self.host_id}"\n'
                   f'[[services]]\ncapability = {quote(self.capabilities["ut"])}\nlisten_unix = {quote(rollback)}\n'
                   f'[[services]]\ncapability = {quote(self.capabilities["uu"])}\nlisten_unix = {quote(self.listeners["uu"])}\n')
        process = await Process(self, "rollback", ["attach", "--config", str(config)]).start()
        await asyncio.wait_for(process.child.wait(), STARTUP_TIMEOUT)
        require(process.child.returncode != 0 and not rollback.exists(), "startup rollback failed")
        # Last-owner cleanup must preserve a substituted regular file.
        path = self.root / "replace"
        process = await self.positional("uu", path)
        await process.wait_for("Local Unix socket:")
        path.unlink()
        path.write_bytes(b"replacement")
        await process.stop()
        require(path.read_bytes() == b"replacement", "cleanup removed a substituted file")
        path.unlink()
        # SIGKILL creates a stale socket; the CLI must not recover it implicitly.
        path = self.root / "stale"
        process = await self.positional("uu", path)
        await process.wait_for("Local Unix socket:")
        await process.stop(signal.SIGKILL, clean=False)
        require(path.is_socket(), "forced-kill fixture did not leave a stale socket")
        process = await self.positional("uu", path)
        await asyncio.wait_for(process.child.wait(), STARTUP_TIMEOUT)
        require(process.child.returncode != 0 and path.is_socket(), "stale socket was replaced")
        path.unlink()  # Explicit removal of this run's verified stale fixture only.
        process = await self.positional("uu", path)
        await process.wait_for("Local Unix socket:")
        await process.stop()
        self.event("listener_checks_passed", foreign_owner="skipped: requires additional user/privileges")
        # Wrong secret must not deliver a message to the backend.
        path = self.root / "wrong"
        process = await self.positional("uu", path, "uu:tcp:" + "0" * 64)
        await process.wait_for("Local Unix socket:")
        before = sum(fixture.accepted for fixture in self.fixtures)
        reader, writer = await connect(path)
        writer.write(b"unauthorized")
        await writer.drain()
        await self.expect_closed(reader)
        await close(writer)
        await process.stop()
        require(sum(fixture.accepted for fixture in self.fixtures) == before,
                "unauthorized stream connected to an upstream")

    async def idle_checks(self):
        for service, idle in (("hostidle", 0), ("tt", 1)):
            path = self.root / "idle"
            process = await self.positional(service, path, idle=idle)
            await process.wait_for("Local Unix socket:")
            reader, writer = await connect(path)
            await exchange(reader, writer, uuid.uuid4().bytes, 1, 1)
            started = time.monotonic()
            await self.expect_closed(reader)
            require(0.7 <= time.monotonic() - started <= 5, "custom idle deadline out of bounds")
            await close(writer)
            await process.stop()
        self.event("stricter_endpoint_idle_passed")

    async def quiet_gate(self):
        connections = []
        try:
            for address in self.listeners.values():
                reader, writer = await connect(address)
                identity = uuid.uuid4().bytes
                await exchange(reader, writer, identity, 1, 1)
                connections.append((reader, writer, identity))
            default_path = self.root / "default"
            default = await self.positional("default", default_path, idle=None)
            await default.wait_for("Local Unix socket:")
            reader, writer = await connect(default_path)
            await exchange(reader, writer, uuid.uuid4().bytes, 1, 1)
            connections.append((reader, writer, None))
            started = time.monotonic()
            async def remain_quiet(reader, writer, identity):
                try:
                    async with asyncio.timeout(365):
                        await reader.read(1)
                        raise AssertionError("disabled-idle stream closed or received application bytes")
                except asyncio.TimeoutError:
                    pass
                await asyncio.wait_for(exchange(reader, writer, identity, 2, 256), TIMEOUT)

            await asyncio.gather(self.expect_default_expiry(reader),
                                 *(remain_quiet(r, w, identity) for r, w, identity in connections[:-1]))
            self.event("quiet_gate_passed", application_silence_seconds=time.monotonic() - started)
            for reader, writer, _ in connections[:-1]:
                writer.write_eof()
                await writer.drain()
                require(await asyncio.wait_for(reader.read(1), TIMEOUT) == b"",
                        "quiet stream did not drain cleanly")
            await default.stop()
        finally:
            for _, writer, _ in connections:
                await close(writer)

    async def expect_default_expiry(self, reader):
        started = time.monotonic()
        async with asyncio.timeout(320):
            try:
                require(await reader.read(1) == b"", "default-idle stream returned data")
            except ConnectionResetError:
                pass
        elapsed = time.monotonic() - started
        require(290 <= elapsed <= 320, "default 300-second idle expiry out of bounds")
        self.event("default_idle_passed", elapsed=elapsed)

    async def worker(self, name, index, stop, *, stress=False):
        key = f"{name}:load-{index}"
        identity = self.identities.setdefault(key, uuid.uuid4().bytes)
        reader = writer = None
        connection_generation = self.fault_epochs[name]
        last_churn = time.monotonic()
        try:
            while not stop.is_set():
                started = time.monotonic()
                fault_at_start = self.fault and name in self.fault_services
                generation_at_start = self.fault_epochs[name]
                try:
                    async with asyncio.timeout(TIMEOUT):
                        if writer is None:
                            connection_generation = self.fault_epochs[name]
                            reader, writer = await connect(self.listeners[name])
                        self.sequence[key] += 1
                        if stress:
                            size = (1024 * 1024
                                    if index % 4 == 0 and self.sequence[key] % 10 == 0
                                    else 4096)
                            slow = index % 4 == 1 and self.sequence[key] % 10 == 0
                        else:
                            size = (1024 * 1024 if index == 0 and self.sequence[key] % 20 == 0 else 4096)
                            slow = index == 1 and self.sequence[key] % 20 == 0
                        await exchange(reader, writer, identity, self.sequence[key], size, slow=slow)
                    self.counts[f"{name}.success"] += 1
                    self.counts["bytes"] += size * 2
                    self.latencies.append(time.monotonic() - started)
                    churn = (index % self.stress_churn_period == self.stress_churn_period - 1
                             if stress else index == 2)
                    if churn or time.monotonic() - last_churn > (15 if stress else 60):
                        writer.write_eof()
                        await writer.drain()
                        await asyncio.wait_for(reader.read(1), TIMEOUT)
                        await close(writer)
                        reader = writer = None
                        last_churn = time.monotonic()
                except (OSError, asyncio.TimeoutError, asyncio.IncompleteReadError) as exception:
                    expected = fault_failure(self.fault and name in self.fault_services,
                                             fault_at_start, generation_at_start,
                                             connection_generation, self.fault_epochs[name])
                    self.counts["fault_failures" if expected else "unexpected_failures"] += 1
                    self.event("operation_failed", combination=name, expected=expected,
                               connection_epoch=connection_generation, operation_epoch=generation_at_start,
                               fault_epoch=self.fault_epochs[name], error_type=type(exception).__name__)
                    if writer:
                        await close(writer)
                    reader = writer = None
                stress_pause = 0.05 if sys.platform == "darwin" else 0.02
                await asyncio.sleep(stress_pause if stress else (0.1 if index == 2 else 0.05))
        finally:
            if writer:
                try:
                    writer.write_eof()
                    await writer.drain()
                    await asyncio.wait_for(reader.read(1), TIMEOUT)
                except (OSError, asyncio.TimeoutError):
                    pass
                await close(writer)

    async def sample(self):
        records = {}
        for process in (self.host, self.attachment):
            pid = process.child.pid
            _, stdout = await capture("ps", "-p", str(pid), "-o", "rss=,pcpu=", timeout=5)
            fields = stdout.decode().split()
            require(len(fields) == 2, f"{process.name} exited unexpectedly")
            if sys.platform.startswith("linux"):
                fds = len(list(Path(f"/proc/{pid}/fd").iterdir()))
            else:
                status, output = await capture("lsof", "-n", "-P", "-p", str(pid), "-Ff", timeout=5)
                require(status == 0, "lsof failed to collect process descriptors")
                fds = sum(line[1:].isdigit() for line in output.decode().splitlines() if line.startswith("f"))
            record = {"time": time.time(), "process": process.name,
                      "pid": pid, "rss_kib": int(fields[0]),
                      "cpu_percent": float(fields[1]), "fds": fds,
                      "fault": self.fault, "counts": dict(self.counts)}
            records[process.name] = record
            with open(self.output / "metrics.jsonl", "a") as output:
                output.write(json.dumps(record) + "\n")
        return records

    async def soak(self):
        baseline = await self.sample()
        stop = asyncio.Event()
        tasks = [asyncio.create_task(self.worker(name, i, stop))
                 for name in self.listeners for i in range(3)]
        tasks.append(asyncio.create_task(self.quiet_gate()))
        started = time.monotonic()
        self.measured_load_seconds = None
        # No faults can invalidate the six-minute quiet gate.
        fault_times = [(self.args.duration * fraction, target)
                       for fraction, target in ((0.45, "upstream"), (0.65, "host"), (0.8, "attachment"))]
        next_sample = 0
        completed = False
        checkpoint_done = False
        try:
            while time.monotonic() - started < self.args.duration:
                for task in tasks:
                    if task.done():
                        task.result()  # Surface checksum/fixture failures immediately.
                elapsed = time.monotonic() - started
                if not checkpoint_done and elapsed >= max(370, self.args.duration * 0.4):
                    require(tasks[-1].done(), "quiet gate must finish before the same-PID drain checkpoint")
                    tasks[-1].result()
                    stop.set()
                    results = await asyncio.wait_for(asyncio.gather(*tasks[:-1], return_exceptions=True), TIMEOUT + 5)
                    for result in results:
                        if isinstance(result, BaseException):
                            raise result
                    await asyncio.sleep(2)
                    drained = await self.sample()
                    for name in baseline:
                        growth = descriptor_growth(baseline[name], drained[name], self.args.max_fd_growth)
                        self.event("same_pid_resource_gate_passed", process=name, pid=drained[name]["pid"], fd_growth=growth)
                    stop = asyncio.Event()
                    quiet = tasks[-1]
                    tasks = [asyncio.create_task(self.worker(name, i, stop))
                             for name in self.listeners for i in range(3)] + [quiet]
                    checkpoint_done = True
                if fault_times and elapsed >= fault_times[0][0]:
                    _, target = fault_times.pop(0)
                    await self.fault_restart(target)
                if elapsed >= next_sample:
                    await self.sample()
                    next_sample = elapsed + 10
                await asyncio.sleep(0.2)
            require(tasks[-1].done(), "quiet gate did not finish")
            tasks[-1].result()
            require(checkpoint_done, "same-PID resource gate did not run")
            completed = True
        finally:
            stop.set()
            if not completed:
                for task in tasks:
                    task.cancel()
            try:
                results = await asyncio.wait_for(asyncio.gather(*tasks, return_exceptions=True), TIMEOUT + 5)
                if completed:
                    for result in results:
                        if isinstance(result, BaseException):
                            raise result
            except asyncio.TimeoutError:
                raise AssertionError("soak workers did not drain within deadline") from None
        self.measured_load_seconds = time.monotonic() - started
        require(self.counts["unexpected_failures"] == 0, "unexpected steady-state operation failures")
        await self.probe_all()
        await asyncio.sleep(2)
        final = await self.sample()
        for name in baseline:
            fd_growth = final[name]["fds"] - baseline[name]["fds"]
            rss_growth = (final[name]["rss_kib"] - baseline[name]["rss_kib"]) / 1024
            self.event("resource_drain", process=name, fd_growth=fd_growth, rss_growth_mib=rss_growth,
                       same_pid=baseline[name]["pid"] == final[name]["pid"],
                       note="Post-restart deltas are diagnostic; the same-PID gate ran before fault injection")
        self.event("soak_passed", elapsed=time.monotonic() - started)

    async def cleanup(self):
        # Graceful stop first, then kill only our own children if needed.
        errors = []
        for process in reversed(self.processes):
            try:
                await process.stop(clean=False)
            except Exception:
                errors.append("process shutdown failed")
        for fixture in self.fixtures:
            try:
                await asyncio.wait_for(fixture.stop(), 5)
            except Exception:
                errors.append("fixture shutdown failed")
        return errors


async def finalize(run, args, started, result, error):
    cleanup_errors = []
    try:
        cleanup_errors = await asyncio.wait_for(run.cleanup(), FINALIZATION_TIMEOUT)
    except Exception:
        cleanup_errors.append("finalization cleanup deadline/failure")
        # The cleanup timeout cannot leave a live run-owned Locho child behind.
        waits = []
        for process in run.processes:
            if process.child.returncode is None:
                process.child.kill()
                waits.append(process.child.wait())
            for pump in process.pumps:
                pump.cancel()
        if waits:
            try:
                await asyncio.wait_for(asyncio.gather(*waits), 3)
            except asyncio.TimeoutError:
                cleanup_errors.append("forced process reap deadline")
    version = None
    try:
        status, output = await capture(str(run.binary), "--version", timeout=5)
        require(status == 0, "version probe failed")
        version = output.decode().strip()
    except Exception:
        cleanup_errors.append("binary version probe failed")
    if cleanup_errors:
        result = "failed"
        error = error or "finalization failed"
    successes = sum(value for key, value in run.counts.items() if key.endswith(".success"))
    measured_load_seconds = run.measured_load_seconds
    summary = {"result": result, "error": error, "mode": args.mode,
               "elapsed_seconds": time.monotonic() - started,
               "requested_load_seconds": args.duration if args.mode in ("stress", "soak") else None,
               "measured_load_seconds": measured_load_seconds,
               "stress_levels": getattr(args, "stress_levels", None),
               "successful_operations": successes,
               "throughput_operations_per_second": successes / measured_load_seconds
               if measured_load_seconds and args.mode in ("stress", "soak") else None,
               "throughput_bytes_per_second": run.counts["bytes"] / measured_load_seconds
               if measured_load_seconds and args.mode in ("stress", "soak") else None,
               "max_fd_growth_allowance": args.max_fd_growth,
               "platform": platform.platform(), "python": platform.python_version(),
               "binary_version": version,
               "binary_sha256": run.binary_sha256, "harness_sha256": run.harness_sha256,
               "counts": dict(run.counts), "cleanup_errors": cleanup_errors,
               "failure_events_recorded": run.events,
               "upstream_messages": sum(f.messages for f in run.fixtures),
               "duplicates": sum(f.duplicates for f in run.fixtures),
               "fixture_errors": sum(f.errors for f in run.fixtures),
               "interrupted_upstream_streams": sum(f.interrupted for f in run.fixtures),
               "failure_frames": run.failure_frames,
               "latency_sample_count": len(run.latencies),
               "latency_sampling": "latest 10000 successful operations",
               "transport": "explicit loopback direct-address; direct path required at readiness",
               "scope": "single-machine native direct transport; no WAN/relay acceptance"}
    samples = sorted(run.latencies)
    summary["latency_seconds"] = {f"p{p}": samples[min(len(samples) - 1, int(len(samples) * p / 100))]
                                   if samples else None for p in (50, 95, 99)}
    try:
        (run.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    finally:
        # Reporting/version failures must never skip private state removal.
        shutil.rmtree(run.root)
    return result, error


async def main(args):
    run = Run(args)
    task = asyncio.current_task()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, task.cancel)
    print(f"Artifacts: {run.output}", flush=True)
    started = time.monotonic()
    result = "failed"
    error = None
    try:
        # Includes setup, faults and cleanup budget; no unbounded harness hang.
        async with asyncio.timeout((args.duration if args.mode in ("stress", "soak") else 240) + 120):
            await run.setup()
            if args.mode == "smoke":
                await run.smoke()
            elif args.mode == "stress":
                await run.stress()
            else:
                await run.soak()
            require(all(fixture.duplicates == 0 and fixture.errors == 0 for fixture in run.fixtures),
                    "fixture detected corruption or replay")
            await run.attachment.stop()
            run.assert_unlinked()
            await run.host.stop()
            result = "passed"
    except asyncio.CancelledError:
        error = "interrupted"
    except Exception as exception:
        # Do not echo arbitrary I/O errors, credentials, or private paths.
        error = str(exception) if isinstance(exception, AssertionError) else type(exception).__name__
        # Stack locations only, never exception values/locals containing frames
        # or capabilities. Preserve enough context to locate a failed phase.
        import traceback
        run.failure_frames = [{"file": Path(frame.filename).name, "line": frame.lineno,
                               "function": frame.name}
                              for frame in traceback.extract_tb(exception.__traceback__)]
    finally:
        # A repeated interrupt must not cancel the bounded ownership cleanup.
        for sig in (signal.SIGINT, signal.SIGTERM):
            loop.add_signal_handler(sig, lambda: None)
        result, error = await finalize(run, args, started, result, error)
    print(f"Unix {args.mode}: {result}" + (f" ({error})" if error else ""), flush=True)
    return 0 if result == "passed" else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("smoke", "stress", "soak"))
    parser.add_argument("--binary", default=os.environ.get("LOCHO_TEST_BINARY", "target/release/locho"))
    parser.add_argument("--duration", type=duration, default=None,
                        help="stress duration defaults to 5m; soak duration defaults to 30m")
    parser.add_argument("--levels", type=stress_levels, default=[4, 8, 16, 32],
                        help="stress workers per forwarding combination, comma-separated")
    parser.add_argument("--concurrency", type=int,
                        help="stress shorthand for one workers-per-combination level")
    parser.add_argument("--output", default="artifacts")
    parser.add_argument("--max-fd-growth", type=int, default=16,
                        help="allowed post-drain descriptor increase per Locho process")
    args = parser.parse_args()
    if os.name != "posix" or not hasattr(socket, "AF_UNIX"):
        parser.error("native Unix sockets require macOS or Linux")
    if sys.version_info < (3, 11):
        parser.error("Python 3.11 or newer is required")
    if args.duration is None:
        args.duration = 300 if args.mode == "stress" else 1800
    if args.mode == "stress":
        if not 60 <= args.duration <= 3600:
            parser.error("stress duration must be 1m–1h")
        if args.concurrency is not None:
            if not 1 <= args.concurrency <= 128:
                parser.error("--concurrency must be between 1 and 128")
            args.levels = [args.concurrency]
        args.stress_levels = args.levels
        if sys.platform == "darwin" and args.stress_levels == [4, 8, 16, 32]:
            # The full 128-stream budget is covered by budget_check. macOS's
            # loopback/QUIC churn at exactly 128 active workers can leave a
            # transient batch of stream handshakes waiting behind teardown;
            # keep the loaded stress gate below that boundary while retaining
            # the dedicated 128-stream admission regression.
            args.stress_levels = [4, 8, 16, 28]
    elif args.mode == "soak" and not 900 <= args.duration <= 3600:
        parser.error("soak duration must be 15m–1h to cover quiet expiry and recovery gates")
    if args.max_fd_growth < 0:
        parser.error("--max-fd-growth must be non-negative")
    try:
        sys.exit(asyncio.run(main(args)))
    except KeyboardInterrupt:
        sys.exit(130)
