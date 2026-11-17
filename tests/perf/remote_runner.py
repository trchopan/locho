#!/usr/bin/env python3
"""Private SSH bridge, detached run-owned supervisor, and deterministic fixtures.

Not an installed daemon. Only the controller stages/invokes this runner. The
supervisor owns the children and survives bridge/SSH loss until its lease expires.
"""

import argparse
import asyncio
import base64
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import sys
import time
import uuid

from native_support import (
    Artifacts, MESSAGE_LIMIT, OwnedProcess, binary_metadata, capacity, encode, environment,
    message, private_json, receive, relay_arguments, validate_message, validate_relay_policy, write_relay_config,
)
from unix_streams import Fixture, capture, quote, require, tcp_port, write_toml


def fixture_snapshot(fixtures):
    return {name: {key: getattr(fixture, key) for key in
                   ("messages", "duplicates", "errors", "interrupted", "accepted")}
            | {"highwater": {identity.hex(): seq for identity, seq in fixture.highwater.items()}}
            for name, fixture in fixtures.items()}


class FixtureObservation:
    """Bounded large-frame observations; no payload or authentication values."""

    def __init__(self, artifacts):
        self.artifacts = artifacts
        self.active = {}

    def callback(self, backend):
        def observe(phase, frame, reader, writer):
            if frame["size"] < 1024 * 1024:
                return
            key = (backend, id(writer))
            if phase == "request_header":
                self.active[key] = dict(backend=backend, **frame, reader=reader, writer=writer,
                                        started=time.monotonic(), request_read_complete=False,
                                        response_drained=False, request_validated=False)
            record = self.active.get(key)
            if record is None:
                return
            record.update({k: v for k, v in frame.items() if k not in ("reader", "writer")})
            record["phase"] = phase
            if phase in ("request_read_complete", "response_drained", "request_validated", "request_eof"):
                record[phase] = True
            self.emit(record, phase)
            # Mode 0 may keep its connection alive for many small frames after a
            # large response. Do not retain/sample that completed large frame,
            # or hold its reader/writer until the next large operation.
            response_done = record["response_drained"] and record["request_validated"]
            if phase == "connection_closing" or response_done and (record["mode"] == 0 or record.get("request_eof")):
                self.active.pop(key, None)
        return observe

    def emit(self, record, phase):
        buffer = getattr(record["reader"], "_buffer", None)
        buffered = len(buffer) if isinstance(buffer, (bytes, bytearray)) else None
        transport = getattr(record["writer"], "transport", None)
        queued = transport.get_write_buffer_size() if transport else None
        value = {key: item for key, item in record.items() if key not in ("reader", "writer", "started")}
        value.update(phase=phase, elapsed_seconds=time.monotonic() - record["started"],
                     buffered_request_bytes=buffered, local_response_write_buffer_bytes=queued,
                     scope="target application I/O; local response drain is not remote receipt")
        self.artifacts.event("fixture-progress.jsonl", value)

    def sample(self):
        for record in list(self.active.values()):
            self.emit(record, "active_sample")


async def fixture_main(root):
    settings = json.loads((root / "fixtures.json").read_text())
    observer = FixtureObservation(Artifacts(root / "artifacts")) if (root / "fixture-observation.json").exists() else None
    fixtures = {name: Fixture(Path(value) if isinstance(value, str) else tuple(value),
                              observer.callback(name) if observer else None)
                for name, value in settings.items()}
    state = root / "fixture-state.json"
    if state.exists():
        saved = json.loads(state.read_text())
        for name, fixture in fixtures.items():
            for key in ("messages", "duplicates", "errors", "interrupted", "accepted"):
                setattr(fixture, key, saved[name][key])
            fixture.highwater = {bytes.fromhex(identity): seq
                                 for identity, seq in saved[name]["highwater"].items()}
    stopped = asyncio.Event()
    for sig in (signal.SIGTERM, signal.SIGINT):
        asyncio.get_running_loop().add_signal_handler(sig, stopped.set)
    try:
        for fixture in fixtures.values():
            await fixture.start()
        print("fixtures ready", flush=True)
        next_observation = 0
        while not stopped.is_set():
            private_json(state, fixture_snapshot(fixtures))
            if observer and time.monotonic() >= next_observation:
                observer.sample()
                next_observation = time.monotonic() + 1
            try:
                await asyncio.wait_for(stopped.wait(), 0.25)
            except asyncio.TimeoutError:
                pass
    finally:
        for fixture in fixtures.values():
            await fixture.stop()
        # Scheduled restarts preserve validation counters and replay high-water.
        private_json(state, fixture_snapshot(fixtures))
        if observer:
            observer.artifacts.json("fixture-observation-summary.json", dict(
                records=dict(observer.artifacts.events), truncated_details=dict(observer.artifacts.truncated),
                active_frames_at_stop=len(observer.active), detail_cap=2000))


class Supervisor:
    def __init__(self, args):
        self.args = args
        self.root = Path(args.root).resolve()
        require(self.root.is_dir() and self.root.stat().st_mode & 0o777 == 0o700,
                "runner requires a private runtime directory")
        owner = json.loads((self.root / "owner.json").read_text())
        require(owner["run_id"] == args.run_id, "runtime ownership mismatch")
        self.artifacts = Artifacts(self.root / "artifacts")
        self.artifacts.runtime_paths.add(str(self.root))
        self.started = time.monotonic()
        self.deadline = self.started + args.budget
        self.heartbeat = self.started
        self.lease = args.lease
        self.processes = {}
        self.initialized = False
        self.stopped = False
        self.cleanup = "unverified"
        self.cleanup_errors = []
        self.cleanup_done = asyncio.Event()
        self.reason = None
        self.lock = asyncio.Lock()
        self.done = asyncio.Event()
        self.server = None
        self.env = None
        self.active = None
        self.cleanup_task = None
        self.remove_requested = False
        self.client_writers = set()

    async def start_process(self, name, argv):
        require(not self.stopped, "watchdog stopped run during setup")
        process = OwnedProcess(name, argv, self.artifacts, self.env,
                               self.settings["fd_soft"], self.settings["fd_hard"],
                                budget=max(1, self.deadline - time.monotonic()))
        process.resource_observation = self.settings.get("resource_observation", False)
        self.processes[name] = process  # Register before startup can fail.
        await process.start()
        return process

    async def initialize(self, settings):
        require(not self.initialized and not self.processes and not self.stopped,
                "run already initialized or stopped")
        fields = {"fd_soft", "fd_hard", "relay_env", "provenance", "commit", "dev_idle"}
        require(fields <= set(settings) <= fields | {"relay_policy", "fixture_observation", "resource_observation"},
                "invalid initialization settings")
        require(type(settings.get("fixture_observation", False)) is bool, "invalid fixture observation flag")
        require(type(settings.get("resource_observation", False)) is bool, "invalid resource observation flag")
        policy = validate_relay_policy(settings.get("relay_policy"))
        require(type(settings["fd_soft"]) is int and type(settings["fd_hard"]) is int
                and 64 <= settings["fd_soft"] <= settings["fd_hard"] <= 65536,
                "invalid child descriptor policy")
        require(type(settings["dev_idle"]) is int and 1 <= settings["dev_idle"] <= 300,
                "invalid development idle fixture")
        self.settings = settings
        preflight = capacity(self.root, settings["fd_soft"], settings["fd_hard"])
        self.env = await environment(self.root, settings["relay_env"], self.artifacts)
        self.relay_config = write_relay_config(self.root, policy, self.env)
        options = relay_arguments(self.relay_config)
        binary = self.root / "locho"
        status, version = await capture(str(binary), "--version", env=self.env, timeout=5)
        require(status == 0, "target binary version probe failed")
        fixtures = {"tt": ["127.0.0.1", tcp_port()], "ut": ["127.0.0.1", tcp_port()],
                    "unix": str(self.root / "u")}
        require(fixtures["tt"][1] != fixtures["ut"][1], "fixture ports collided")
        private_json(self.root / "fixtures.json", fixtures)
        if settings.get("fixture_observation"):
            private_json(self.root / "fixture-observation.json", {"enabled": True})
        fixture = await self.start_process("fixtures", [sys.executable, self.root / "remote_runner.py",
                                                        "--fixture", "--root", self.root])
        await fixture.ready("fixtures ready")
        services = []
        for name in ("tt", "tu", "ut", "uu", "default", "shortidle"):
            if name.endswith("u"):
                endpoint = f'unix_socket = {quote(self.root / "u")}'
            else:
                address = fixtures["ut" if name == "ut" else "tt"]
                endpoint = f'endpoint = "{address[0]}:{address[1]}"'
            idle = ("" if name == "default" else
                    f'tcp_idle_timeout_secs = {settings["dev_idle"] if name == "shortidle" else 0}\n')
            services.append(f'[[services]]\nname = "{name}"\ntype = "tcp"\n{endpoint}\n{idle}')
        write_toml(self.root / "host.toml", "\n".join(services))
        # Normal discovery/relay selection: no direct-address test override.
        host = await self.start_process("host", [binary, "host", "--config", self.root / "host.toml", *options])
        await host.ready("Services:")
        status, share = await capture(str(binary), "share", "tt", "--config",
                                      str(self.root / "host.toml"), *options, env=self.env, timeout=5)
        require(status == 0 and len(share.split()) >= 3, "test host identity creation failed")
        host_id = share.decode().split()[2]
        capabilities = {}
        for name in ("tt", "tu", "ut", "uu", "default", "shortidle"):
            status, output = await capture(str(binary), "secret", name, "--config",
                                           str(self.root / "host.toml"), env=self.env, timeout=5)
            require(status == 0, "test capability creation failed")
            capabilities[name] = output.decode().strip()
            self.artifacts.secrets.update((capabilities[name], capabilities[name].split(":")[-1]))
        metadata = dict(binary=binary_metadata(binary, settings["provenance"], settings["commit"])
                        | {"version": version.decode().strip()}, os=platform.platform(),
                        architecture=platform.machine(), python=platform.python_version(),
                        capacity=preflight,
                        relay_policy=policy, relay_token_available=bool(policy and self.env.get(policy["token_env"])),
                        fixtures=fixtures, fd_policy={"soft": settings["fd_soft"], "hard": settings["fd_hard"]})
        self.artifacts.json("metadata.json", metadata)
        self.initialized = True
        self.heartbeat = time.monotonic()
        return dict(host_id=host_id, capabilities=capabilities, metadata=metadata)

    def event(self, event, **fields):
        self.artifacts.event("timeline.jsonl", dict(event=event, **fields))

    async def stop(self, reason="controller"):
        if self.stopped:
            return await asyncio.shield(self.cleanup_task)
        self.stopped = True
        self.reason = reason
        self.cleanup_task = asyncio.create_task(self._stop(reason))
        return await asyncio.shield(self.cleanup_task)

    async def _stop(self, reason):
        try:
            async with asyncio.timeout(30):
                for process in reversed(list(self.processes.values())):
                    try:
                        await process.stop()
                    except Exception as error:
                        self.cleanup_errors.append(type(error).__name__)
        except asyncio.TimeoutError:
            self.cleanup_errors.append("cleanup deadline exceeded")
        # Run force-reap concurrently under one fallback budget, not per child.
        try:
            async with asyncio.timeout(5):
                await asyncio.gather(*(p.force_reap() for p in self.processes.values()))
        except Exception as error:
            self.cleanup_errors.append(type(error).__name__)
        self.cleanup = ("verified" if all(p.child is None or p.child.returncode is not None
                                        for p in self.processes.values()) else "unverified")
        state = self.root / "fixture-state.json"
        counters = None
        try:
            if state.exists():
                counters = {name: {key: value for key, value in record.items() if key != "highwater"}
                            for name, record in json.loads(state.read_text()).items()}
                if any(record["errors"] or record["duplicates"] for record in counters.values()):
                    self.cleanup_errors.append("fixture corruption or replay")
        except Exception:
            self.cleanup_errors.append("final fixture counters unavailable")
        # Remove all generated credentials/state immediately, even if collection
        # is impossible. Retain only private sanitized diagnostics and runner code.
        for path in (self.root / "state", self.root / "host.toml", self.root / "relay.toml", self.root / "fixtures.json",
                     self.root / "fixture-observation.json",
                     self.root / "fixture-state.json"):
            try:
                if path.is_dir():
                    shutil.rmtree(path)
                elif path.exists():
                    path.unlink()
            except Exception:
                self.cleanup_errors.append("private runtime material removal failed")
        # Reporting failure must never prevent credential removal or child reaping.
        try:
            if counters is not None:
                self.artifacts.json("fixture-summary.json", counters)
            self.event("cleanup", status=self.cleanup, reason=reason, errors=self.cleanup_errors)
            self.artifacts.json("cleanup.json", dict(run_id=self.args.run_id, cleanup=self.cleanup,
                                                    errors=self.cleanup_errors, reason=reason,
                                                    processes={name: dict(pid=p.pid, guard_pid=p.child.pid if p.child else None,
                                                                generation=p.generation, start_identity=p.start_identity,
                                                                exit_status=p.child.returncode if p.child else None,
                                                                log_categories=dict(p.categories))
                                                               for name, p in self.processes.items()}))
        except Exception:
            self.cleanup_errors.append("sanitized cleanup report failed")
        self.cleanup_done.set()
        return dict(cleanup=self.cleanup, errors=self.cleanup_errors)

    async def dispatch(self, command, payload):
        if command == "heartbeat":
            require(not self.stopped, "watchdog already stopped workload")
            self.heartbeat = time.monotonic()
            return dict(utc=time.time(), monotonic=time.monotonic())
        if command == "initialize":
            return await self.initialize(payload)
        if command == "status":
            return dict(cleanup=self.cleanup, stopped=self.stopped, reason=self.reason,
                        processes={name: dict(pid=p.pid,
                                              generation=p.generation, start_identity=p.start_identity,
                                              exit_status=p.child.returncode if p.child else None)
                                   for name, p in self.processes.items()})
        if command == "stop":
            return await self.stop()
        if command == "manifest":
            require(self.stopped and self.cleanup_done.is_set(), "finish cleanup before artifact collection")
            return {str(path.relative_to(self.artifacts.root)):
                    dict(size=path.stat().st_size, sha256=hashlib.sha256(path.read_bytes()).hexdigest())
                    for path in self.artifacts.root.rglob("*") if path.is_file()}
        if command == "artifact":
            require(self.stopped, "stop workload before artifact collection")
            name, offset = payload.get("name"), payload.get("offset")
            require(isinstance(name, str) and type(offset) is int and offset >= 0, "invalid artifact request")
            path = (self.artifacts.root / name).resolve()
            require(path.is_relative_to(self.artifacts.root) and path.is_file() and not path.is_symlink(),
                    "artifact outside owned directory")
            with open(path, "rb") as source:
                source.seek(offset)
                return dict(data=base64.b64encode(source.read(128 * 1024)).decode())
        if command == "finish":
            require(self.stopped and self.cleanup_done.is_set() and self.cleanup == "verified", "cleanup not verified")
            self.remove_requested = True
            self.done.set()
            return dict(cleanup="verified", runtime_removal="requested")
        require(self.initialized and not self.stopped, "workload is not active")
        self.heartbeat = time.monotonic()
        if command == "sample":
            records = {name: await p.sample() for name, p in self.processes.items()}
            for record in records.values():
                self.artifacts.event("metrics-target.jsonl", record, cap=20000)
            state = json.loads((self.root / "fixture-state.json").read_text())
            return dict(records=records, fixtures=state)
        if command == "restart":
            name = payload.get("process")
            require(name in ("host", "fixtures"), "invalid fault scope")
            process = self.processes[name]
            require(process.child.returncode is None, "unexpected exit before scheduled fault")
            self.event("fault_start", process=name, generation=process.generation)
            await process.stop()
            require(process.child.returncode == 0, "scheduled target stop failed")
            await process.start()
            await process.ready("Services:" if name == "host" else "fixtures ready")
            self.heartbeat = time.monotonic()
            self.event("fault_ready", process=name, generation=process.generation)
            return dict(generation=process.generation)
        raise AssertionError("unknown protocol command")

    async def client(self, reader, writer):
        self.client_writers.add(writer)
        try:
            while True:
                request = validate_message(await receive(reader), self.args.run_id)
                try:
                    async with self.lock:
                        async with asyncio.timeout(min(90, max(0.01, self.deadline - time.monotonic()))):
                            self.active = asyncio.create_task(self.dispatch(request["command"], request["payload"]))
                            try:
                                result = await self.active
                            finally:
                                self.active = None
                    response = dict(version=1, run_id=self.args.run_id, id=request["id"], ok=True, payload=result)
                except Exception as error:
                    # Error text may include paths/credentials; expose only type.
                    response = dict(version=1, run_id=self.args.run_id, id=request["id"], ok=False,
                                    error=type(error).__name__, payload={})
                    if request["command"] not in ("artifact", "manifest", "status", "finish"):
                        await self.stop("protocol or operation failure")
                writer.write(encode(response))
                await writer.drain()
        except asyncio.CancelledError:
            pass
        except EOFError:
            pass
        except Exception:
            # Lost control does not imply cleanup. The independent lease enforces it.
            await self.stop("invalid protocol")
        finally:
            writer.close()
            self.client_writers.discard(writer)
            await writer.wait_closed()

    async def watch(self):
        while not self.done.is_set():
            now = time.monotonic()
            reason = ("hard deadline" if now >= self.deadline - 35 else
                      "heartbeat expired" if self.initialized and now - self.heartbeat >= self.lease else
                      "setup deadline" if not self.initialized and now - self.started >= 120 else None)
            if reason and not self.stopped:
                if self.active and not self.active.done():
                    self.active.cancel()
                    await asyncio.gather(self.active, return_exceptions=True)
                await self.stop(reason)
            # Bound diagnostic retention after loss; keep a small cleanup receipt
            # in the run directory, so later SSH inspection can verify ownership.
            if self.stopped and now >= self.deadline:
                self.done.set()
            await asyncio.sleep(0.1)

    async def run(self):
        self.server = await asyncio.start_unix_server(self.client, str(self.root / "c"), limit=MESSAGE_LIMIT)
        watcher = asyncio.create_task(self.watch())
        for sig in (signal.SIGTERM, signal.SIGINT):
            asyncio.get_running_loop().add_signal_handler(
                sig, lambda: asyncio.create_task(self.stop("supervisor interrupted")))
        await self.done.wait()
        self.server.close()
        try:
            await asyncio.wait_for(self.server.wait_closed(), 2)
        except asyncio.TimeoutError:
            # Python 3.12+ waits for active clients as well as the listener. A
            # stalled client must not keep a finished watchdog alive indefinitely.
            for writer in list(self.client_writers):
                writer.close()
            await asyncio.wait_for(self.server.wait_closed(), 2)
        watcher.cancel()
        await asyncio.gather(watcher, return_exceptions=True)
        if self.cleanup == "verified" and self.remove_requested:
            shutil.rmtree(self.root)
        else:
            # Abandoned runs retain sanitized diagnostics, never test secrets.
            for name in ("locho", "c"):
                path = self.root / name
                if path.exists():
                    path.unlink()


async def stdin_reader():
    reader = asyncio.StreamReader(limit=MESSAGE_LIMIT)
    protocol = asyncio.StreamReaderProtocol(reader)
    await asyncio.get_running_loop().connect_read_pipe(lambda: protocol, sys.stdin.buffer)
    return reader


async def bridge(args):
    root = Path(args.root).resolve()
    owner = json.loads((root / "owner.json").read_text())
    require(owner["run_id"] == args.run_id, "bridge runtime ownership mismatch")
    if args.inspect:
        owner = json.loads((root / "owner.json").read_text())
        require(owner["run_id"] == args.run_id, "inspection ownership mismatch")
        receipt = root / "artifacts" / "cleanup.json"
        require(receipt.is_file(), "cleanup is still unverified")
        print(receipt.read_text(), flush=True)
        return
    # The watchdog's independent session and closed inherited stdio are essential.
    child = None
    if not args.connect:
        child = await asyncio.create_subprocess_exec(
            sys.executable, str(root / "remote_runner.py"), "--watchdog", "--root", str(root),
            "--run-id", args.run_id, "--budget", str(args.budget), "--lease", str(args.lease),
            start_new_session=True, stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.DEVNULL)
    async with asyncio.timeout(5):
        while not (root / "c").exists():
            require(child is None or child.returncode is None, "watchdog startup failed")
            await asyncio.sleep(0.05)
    remote_reader, remote_writer = await asyncio.open_unix_connection(str(root / "c"), limit=MESSAGE_LIMIT)
    reader = await stdin_reader()
    try:
        while True:
            request = validate_message(await receive(reader), args.run_id)
            remote_writer.write(encode(request))
            await remote_writer.drain()
            response = await receive(remote_reader)
            sys.stdout.buffer.write(encode(response))
            sys.stdout.buffer.flush()
            if request["command"] == "finish":
                # Close the control client before waiting for the supervisor,
                # whose Server.wait_closed() waits for this client to disconnect.
                remote_writer.close()
                await remote_writer.wait_closed()
                if child:
                    await asyncio.wait_for(child.wait(), 5)
                    require(child.returncode == 0, "watchdog finalization failed")
                async with asyncio.timeout(5):
                    while root.exists():
                        await asyncio.sleep(0.05)
                break
    finally:
        remote_writer.close()
        await remote_writer.wait_closed()


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--root", required=True)
    result.add_argument("--run-id", default="")
    result.add_argument("--budget", type=float, default=300)
    result.add_argument("--lease", type=float, default=60)
    result.add_argument("--watchdog", action="store_true")
    result.add_argument("--fixture", action="store_true")
    result.add_argument("--inspect", action="store_true")
    result.add_argument("--connect", action="store_true", help="reconnect to existing owned watchdog")
    return result


if __name__ == "__main__":
    arguments = parser().parse_args()
    if not arguments.fixture:
        require(str(uuid.UUID(arguments.run_id)) == arguments.run_id, "invalid run UUID")
        require(0 < arguments.budget <= 120 + 3600 + 30 + 5 and 1 <= arguments.lease <= 120,
                "invalid watchdog budgets")
    try:
        asyncio.run(fixture_main(Path(arguments.root)) if arguments.fixture else
                    Supervisor(arguments).run() if arguments.watchdog else bridge(arguments))
    except Exception as error:
        print(f"remote runner failed: {type(error).__name__}", file=sys.stderr)
        sys.exit(1)
