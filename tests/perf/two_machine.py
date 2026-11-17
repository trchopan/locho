#!/usr/bin/env python3
"""Opt-in native controller for isolated cross-machine stress and soak tests."""

import argparse
import asyncio
import base64
import collections
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import signal
import sys
import time
import uuid

from native_support import (
    Artifacts, MESSAGE_LIMIT, OwnedProcess, binary_metadata, capacity, encode, environment,
    custom_relay_observed, message, receive, relay_arguments, relay_policy, same_lifetime_growth,
    TransferProgress, write_relay_config,
)
from unix_streams import (
    capture, close, connect, duplex_exchange, duration, exchange, fault_failure,
    quote, require, short_root, tcp_port, write_toml,
)

SERVICES = ("tt", "tu", "ut", "uu")
SETUP = 120
CLEANUP = 30
FORCE_REAP = 5
RECOVERY = 30
BOOTSTRAP = r'''
import base64,json,os,pathlib,shutil,sys,tempfile,uuid
if sys.version_info < (3,11): raise RuntimeError("Python 3.11+ required")
settings=json.loads(sys.stdin.buffer.readline(2097153))
if str(uuid.UUID(settings["run_id"])) != settings["run_id"]: raise RuntimeError("invalid run UUID")
root=pathlib.Path(tempfile.mkdtemp(prefix="lt-",dir=settings["parent"])).resolve()
root.chmod(0o700)
try:
    if len(os.fsencode(root/"c")) > 103: raise RuntimeError("target Unix socket path too long")
    for name,data in settings["files"].items():
        if name not in ("remote_runner.py","native_support.py","unix_streams.py"): raise RuntimeError("invalid stage file")
        fd=os.open(root/name,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
        with os.fdopen(fd,"wb") as out: out.write(base64.b64decode(data,validate=True))
    source=pathlib.Path(settings["binary"]).expanduser().resolve()
    shutil.copyfile(source,root/"locho"); (root/"locho").chmod(0o700)
    fd=os.open(root/"owner.json",os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
    with os.fdopen(fd,"w") as out: json.dump({"run_id":settings["run_id"]},out)
except BaseException:
    shutil.rmtree(root); raise
print(json.dumps({"run_id":settings["run_id"],"root":str(root)}),flush=True)
os.execv(sys.executable,[sys.executable,str(root/"remote_runner.py"),"--root",str(root),"--run-id",settings["run_id"],"--budget",str(settings["budget"]),"--lease","60"])
'''


def levels(value):
    try:
        result = [int(part) for part in value.split(",")]
    except ValueError:
        raise argparse.ArgumentTypeError("use comma-separated total concurrency") from None
    if not result or any(item < 4 or item > 120 for item in result) or result != sorted(set(result)):
        raise argparse.ArgumentTypeError("levels must be increasing unique totals between 4 and 120")
    return result


def distribution(total):
    return {name: total // 4 + (index < total % 4) for index, name in enumerate(SERVICES)}


def mixed_flows(duplex_count):
    require(duplex_count in (0, 4, 7), "mixed diagnostic requires 0, 4 or 7 duplex flows")
    result = [(name, 0, False) for name in SERVICES]
    if duplex_count:
        result += [(name, 1, True) for name in SERVICES]
    if duplex_count == 7:
        result += [(name, 2, True) for name in ("tt", "tu", "ut")]
    return result


def staggered_offsets():
    # Application-operation offsets reconstructed from the failed dev soak.
    # Ordering follows mixed_flows(7); no payload/identity is replayed.
    return [0.129, 0.002, 0.132, 0.0, 1.493, 3.533, 6.593, 1.028, 8.810, 4.766, 8.832]


class RoundStarts:
    """Release warmed connections against one shared monotonic epoch."""

    def __init__(self, count):
        self.barrier = asyncio.Barrier(count)
        self.epoch = None

    async def wait(self):
        await self.barrier.wait()
        if self.epoch is None:
            self.epoch = time.monotonic()
        return self.epoch


def admin_bulk_slots(seconds):
    result = []
    offset = 30
    while offset < seconds - 15:
        result.append(dict(index=len(result), scheduled_offset_seconds=offset,
                           combination=SERVICES[len(result) % len(SERVICES)]))
        offset += 60
    return result


def schedule(args):
    admin = getattr(args, "workload", "capacity") == "supported-admin"
    if admin:
        require(args.mode != "diagnostic", "supported-admin is not a capacity diagnostic")
        require(args.concurrency == 4 and args.levels in (None, [4]) and args.pace == 0.1,
                "supported-admin requires --concurrency 4, levels unset or 4, and pace 0.1")
    if args.mode == "diagnostic":
        seconds = args.duration or 150
        require(args.profile == "dev" and 150 <= seconds <= 180,
                "diagnostic requires a 150–180s dev workload budget")
        kind = getattr(args, "diagnostic_kind", "fragmentation")
        if kind in ("mixed", "staggered", "transport"):
            seconds = args.duration or 180
            require(seconds == 180, "mixed diagnostic requires a fixed 180s workload budget")
            result = dict(duration=seconds, levels=[12], concurrency=12, quiet_seconds=0, checkpoint=0,
                        faults=[], setup_budget=SETUP, cleanup_budget=CLEANUP, forced_reap_budget=FORCE_REAP,
                        outer_budget=SETUP + seconds + CLEANUP + FORCE_REAP,
                        cases=["A_ordinary_first", "B_intermediate_first", "C_high_first",
                               "C_high_second", "B_intermediate_second", "A_ordinary_second"],
                        diagnostic_kind=kind, ordinary_large_streams=4, duplex_large_streams=[0, 4, 7],
                        foreground_large_streams=[4, 8, 11], background_workers=[8, 4, 1],
                        probe=["uu", 2], probe_payload_bytes=1, operation_timeout=10,
                        case_policy="fresh-ID independent rounds, synchronized starts, no restarts; retain failures, never replay")
            if kind == "staggered":
                result.update(cases=["sync_first", "staggered_first", "staggered_second", "sync_second"],
                              duplex_large_streams=[7], foreground_large_streams=[11], background_workers=[1],
                              staggered_start_offsets_seconds=staggered_offsets(),
                              max_start_lateness_seconds=0.25,
                              resource_observation=True,
                              case_policy="fresh IDs; sync-stagger-stagger-sync; actual monotonic starts; no forced overlap, restarts or replay")
            if kind == "transport":
                result.update(cases=["initial_high", "conditioned_high", "post_quiet_high"],
                              duplex_large_streams=[7], foreground_large_streams=[11], background_workers=[1],
                              conditioning="four sequential ordinary 1 MiB exchanges, one per forwarding combination",
                              quiet_seconds=15, resource_observation=True, max_start_lateness_seconds=0.25,
                              peer_lifetimes=2,
                              case_policy="initial-conditioned-postquiet within each of two fresh peer lifetimes; persistent checked probe; no restart/rebaseline, no replay")
                if getattr(args, "no_conditioning_control", False):
                    result.update(cases=["initial_high", "unconditioned_high", "post_quiet_high"],
                                  conditioning="none: five-second wait with tiny probe and observation only",
                                  control_wait_seconds=5,
                                  case_policy="two fresh no-conditioning lifetimes; approximate elapsed-time match to earlier 5.02/4.61s conditioning; not randomized")
            return result
        cases = (["before_two_duplex", "before_three_duplex", "after_two_duplex", "after_three_duplex"]
                 if kind == "duplex" else ["before_unfragmented", "before_fragmented", "after_unfragmented", "after_fragmented"])
        return dict(duration=seconds, levels=[12], concurrency=12, quiet_seconds=0, checkpoint=0,
                    faults=[("after_baseline_cases", "attachment")], setup_budget=SETUP,
                    cleanup_budget=CLEANUP, forced_reap_budget=FORCE_REAP,
                    outer_budget=SETUP + seconds + CLEANUP + FORCE_REAP,
                    cases=cases, diagnostic_kind=kind,
                    background_workers=[10, 9] if kind == "duplex" else 7,
                    foreground_large_streams=[2, 3] if kind == "duplex" else 5, operation_timeout=10,
                    case_policy="independent fresh-ID cases; retain failures, never replay; fail overall if any case fails")
    if args.mode == "smoke":
        seconds = args.duration or 20
        require(args.profile == "dev" and 10 <= seconds <= 60, "smoke requires a 10–60s dev workload budget")
        return dict(duration=seconds, levels=[4], concurrency=4, quiet_seconds=0, checkpoint=0, faults=[],
                    setup_budget=SETUP, cleanup_budget=CLEANUP, forced_reap_budget=FORCE_REAP,
                    outer_budget=SETUP + seconds + CLEANUP + FORCE_REAP)
    seconds = args.duration
    if seconds is None:
        seconds = {("stress", "dev"): 120, ("soak", "dev"): 180,
                   ("stress", "acceptance"): 1200, ("soak", "acceptance"): 3600}[args.mode, args.profile]
    if args.profile == "dev":
        require((60 <= seconds <= 120) if args.mode == "stress" else (120 <= seconds <= 300),
                "dev stress requires 60–120s; dev soak requires 120–300s")
    else:
        require(seconds == (1200 if args.mode == "stress" else 3600),
                "acceptance requires 20m stress or 1h soak")
    selected = [4] if admin else args.levels or ([4, 8] if args.profile == "dev" else [4, 8, 16, 32, 64])
    if args.mode == "stress" and not admin:
        require(len(selected) >= 2 and seconds / len(selected) >= 20,
                "stress requires at least two levels and 20s per level")
    total = max(selected) if args.mode == "stress" else args.concurrency
    require(4 <= total and total + 9 < 128, "traffic, quiet gates and probes must stay below 128 streams")
    quiet = 365 if args.profile == "acceptance" else 8
    checkpoint = max(quiet + 5, seconds * 0.4) if args.mode == "soak" else seconds - 18
    faults = ([] if args.mode == "stress" else
              [(seconds * fraction, target) for fraction, target in
               (((0.45, "fixtures"), (0.65, "host"), (0.8, "attachment"))
                if args.profile == "acceptance" else ((0.6, "attachment"),))])
    if faults:
        require(checkpoint + 20 < faults[0][0] and faults[-1][0] + RECOVERY < seconds,
                "duration cannot fit quiet gate, drain and bounded recovery")
    result = dict(duration=seconds, levels=selected, concurrency=total, quiet_seconds=quiet,
                checkpoint=checkpoint, faults=faults, setup_budget=SETUP,
                cleanup_budget=CLEANUP, forced_reap_budget=FORCE_REAP,
                 outer_budget=SETUP + seconds + CLEANUP + FORCE_REAP)
    if admin:
        result.update(workload="supported-admin", contract_version=1,
                      role="supported-workload endurance, not a capacity ramp",
                      ordinary_payload_bytes=[1, 256, 16384], operation_deadline_seconds=10,
                      bulk_payload_bytes=1048576, bulk_first_seconds=30, bulk_period_seconds=60,
                      bulk_max_active=1, bulk_kind="ordinary unfragmented checked request/response; not duplex or delayed-read",
                      bulk_slots=admin_bulk_slots(seconds), expected_bulk_operations=len(admin_bulk_slots(seconds)),
                      limitations="synthetic native TCP/Unix bytes; not real SSH or HTTP/API semantic certification")
    return result


def dry_run(args, profile, run_id):
    return dict(run_id=run_id, mode=args.mode, profile=args.profile, certification=False,
                workload=getattr(args, "workload", "capacity"),
                suite_role="supported_workload_acceptance" if getattr(args, "workload", "capacity") == "supported-admin" else "capacity_characterization",
                product_release_certification=False,
                acceptance_requested=args.profile == "acceptance",
                binary_profile=args.binary_profile,
                controller_binary=args.controller_binary, target_binary=args.target_binary,
                binary_provenance=dict(controller=args.controller_provenance, target=args.target_provenance),
                control=dict(host=args.ssh_host, port=args.ssh_port, user=args.ssh_user,
                             kind="administrative SSH/Unix bridge" if getattr(args, "control_socket", None) else "controller-to-target SSH",
                             socket=getattr(args, "control_socket", None),
                             python=args.target_python, batch_auth=True, strict_host_keys=True,
                             known_hosts=args.known_hosts, connect_timeout=10),
                target_runtime_parent=args.target_runtime_parent,
                staging="fresh private controller/target directories; copy binaries, never install",
                endpoints={"controller": "dynamic loopback TCP ports and private Unix sockets",
                           "target": "dynamic loopback TCP fixtures and private Unix socket; native Locho dynamic UDP bind"},
                transport="normal discovery/relay candidates; observed paths, not forced direct/relay",
                relay_policy=relay_policy(args),
                transfer_telemetry=getattr(args, "transfer_telemetry", False)
                    or getattr(args, "fixture_observation", False) or args.mode == "diagnostic",
                fixture_observation=getattr(args, "fixture_observation", False) or (
                    args.mode == "diagnostic" and getattr(args, "diagnostic_kind", "fragmentation") in ("duplex", "mixed", "staggered", "transport")),
                schedule=profile,
                distribution=[distribution(n) for n in profile["levels"]] if args.mode == "stress"
                else distribution(profile["concurrency"]), pacing_seconds=args.pace,
                max_payload_bytes=1024 * 1024,
                max_outstanding_payload_bytes=(profile["concurrency"] + 9) * 2 * 1024 * 1024,
                fd_policy={"soft": args.fd_soft, "hard": args.fd_hard, "drain_allowance": args.max_fd_growth},
                fd_baseline_policy=(dict(consecutive_equal_samples=3, minimum_observation_seconds=3,
                                         sample_pause_seconds=1, maximum_samples=10, budget_seconds=15,
                                         scope="setup before workload; same process lifetimes; fail if unstable")
                                    if getattr(args, "workload", "capacity") == "supported-admin" else
                                    dict(scope="existing single warmed setup sample")),
                cleanup="TERM then owned-child KILL; watchdog on target; transfer verified sanitized artifacts; remove owned runtime",
                notes=["Offline preview: target paths/ports are resolved only during approved setup.",
                       "No production listeners, identities, backends, launch agents or network settings are changed.",
                       "No sleep assertion is used; AC/lid state is unverified and must be reviewed."])


class SSHControl:
    def __init__(self, args, run_id, profile):
        self.args, self.run_id, self.profile = args, run_id, profile
        self.child = None
        self.root = None
        self.lock = asyncio.Lock()
        self.request_id = 0
        self.stderr_task = None
        self.stderr_bytes = 0
        self.broken = False

    async def drain_stderr(self):
        # SSH diagnostics are not retained: bootstrap errors can contain private
        # paths or environment output. Store only their bounded byte count.
        while data := await self.child.stderr.read(4096):
            self.stderr_bytes = min(self.stderr_bytes + len(data), 1024 * 1024)

    def ssh_argv(self, command):
        args = self.args
        require(args.ssh_user and args.target_binary and args.controller_binary,
                "execution requires SSH user and both binary paths")
        require(not args.ssh_host.startswith("-") and not args.ssh_user.startswith("-")
                and "\n" not in args.ssh_host + args.ssh_user, "invalid SSH destination")
        argv = ["ssh", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
                "-o", "ConnectTimeout=10", "-o", "ConnectionAttempts=1", "-o", "ServerAliveInterval=5",
                "-o", "ServerAliveCountMax=2", "-p", str(args.ssh_port), "-l", args.ssh_user]
        if args.known_hosts:
            argv.extend(["-o", f"UserKnownHostsFile={Path(args.known_hosts).resolve()}"])
        argv.extend([args.ssh_host, command])
        return argv

    async def start(self):
        args = self.args
        command = shlex.join([args.target_python, "-c", BOOTSTRAP])
        argv = self.ssh_argv(command)
        self.child = await asyncio.create_subprocess_exec(
            *argv, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE, limit=MESSAGE_LIMIT)
        self.stderr_task = asyncio.create_task(self.drain_stderr())
        directory = Path(__file__).parent
        files = {name: base64.b64encode((directory / name).read_bytes()).decode()
                 for name in ("remote_runner.py", "native_support.py", "unix_streams.py")}
        stage = dict(files=files, binary=args.target_binary, parent=args.target_runtime_parent,
                     run_id=self.run_id, budget=self.profile["outer_budget"])
        self.child.stdin.write(encode(stage))
        await self.child.stdin.drain()
        async with asyncio.timeout(20):
            response = await receive(self.child.stdout)
        require(response.get("run_id") == self.run_id and isinstance(response.get("root"), str),
                "invalid bootstrap receipt")
        self.root = response["root"]

    async def rpc(self, command, payload=None, timeout=30):
        async with self.lock:
            try:
                async with asyncio.timeout(timeout):
                    require(not self.broken and self.child and self.child.returncode is None, "SSH control disconnected")
                    self.request_id += 1
                    self.child.stdin.write(encode(message(self.run_id, self.request_id, command, payload)))
                    await self.child.stdin.drain()
                    result = await receive(self.child.stdout)
                    require(result.get("version") == 1 and result.get("run_id") == self.run_id
                            and result.get("id") == self.request_id and type(result.get("ok")) is bool
                            and isinstance(result.get("payload"), dict), "invalid protocol response")
                    require(result["ok"], f"target command failed: {result.get('error', 'protocol error')}")
                    return result["payload"]
            except BaseException:
                self.broken = True
                raise

    async def reconnect(self):
        require(self.root, "no owned target runtime to reconnect")
        await self.close()
        command = shlex.join([self.args.target_python, str(Path(self.root) / "remote_runner.py"),
                             "--connect", "--root", self.root, "--run-id", self.run_id])
        self.child = await asyncio.create_subprocess_exec(
            *self.ssh_argv(command), stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE, limit=MESSAGE_LIMIT)
        self.stderr_task = asyncio.create_task(self.drain_stderr())
        self.broken = False
        await self.rpc("status", timeout=5)

    async def close(self):
        if self.child:
            if self.child.stdin:
                self.child.stdin.close()
            if self.child.returncode is None:
                self.child.terminate()
                try:
                    await asyncio.wait_for(self.child.wait(), 2)
                except asyncio.TimeoutError:
                    self.child.kill()
                    await asyncio.wait_for(self.child.wait(), 3)
        if self.stderr_task:
            self.stderr_task.cancel()
            await asyncio.gather(self.stderr_task, return_exceptions=True)


class SocketControl:
    """RPC over a private Unix socket carried by verified SSH connections.

    The administrative bridge owns target SSH authentication. No private SSH key
    or forwarded authentication agent is needed on the workload controller.
    """

    def __init__(self, args, run_id, profile):
        self.args, self.run_id = args, run_id
        self.root = None
        self.reader = self.writer = None
        self.lock = asyncio.Lock()
        self.request_id = 0
        self.broken = False
        self.finished = False
        self.child = self  # lifecycle facade expected by Run.cleanup
        self.returncode = None

    async def start(self):
        self.reader, self.writer = await asyncio.open_unix_connection(self.args.control_socket, limit=MESSAGE_LIMIT)
        reply = await self.rpc("stage", timeout=30)
        require(isinstance(reply.get("root"), str), "invalid control bridge staging receipt")
        self.root = reply["root"]

    async def rpc(self, command, payload=None, timeout=30):
        async with self.lock:
            try:
                async with asyncio.timeout(timeout):
                    require(not self.broken and self.writer is not None, "control bridge disconnected")
                    self.request_id += 1
                    self.writer.write(encode(message(self.run_id, self.request_id, command, payload)))
                    await self.writer.drain()
                    result = await receive(self.reader)
                    require(result.get("version") == 1 and result.get("run_id") == self.run_id
                            and result.get("id") == self.request_id and result.get("ok") is True
                            and isinstance(result.get("payload"), dict), "control bridge command failed")
                    if command == "finish":
                        self.finished = True
                    return result["payload"]
            except BaseException:
                self.broken = True
                raise

    async def reconnect(self):
        await self.close()
        self.broken = False
        await self.start()
        await self.rpc("status", timeout=5)

    async def wait(self):
        require(self.finished, "target runtime removal not confirmed by bridge")
        self.returncode = 0
        return 0

    async def close(self):
        if self.writer:
            self.writer.close()
            await self.writer.wait_closed()
            self.writer = None


def release_version(text):
    match = re.match(r"^locho\s+(\d+\.\d+\.\d+)(?:\s|$)", text)
    return match.group(1) if match else None


class Run:
    def __init__(self, args, profile, run_id):
        self.args, self.profile, self.run_id = args, profile, run_id
        stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        output = Path(args.output).resolve() / f"two-machine-{args.mode}-{stamp}-{run_id}"
        self.artifacts = Artifacts(output)
        for name in ("timeline.jsonl", "operations.jsonl", "metrics-controller.jsonl", "metrics-target.jsonl"):
            with open(output / name, "x", opener=lambda p, f: os.open(p, f, 0o600)):
                pass
        self.root = short_root()
        self.artifacts.runtime_paths.add(str(self.root))
        self.control = (SocketControl if getattr(args, "control_socket", None) else SSHControl)(args, run_id, profile)
        self.attachment = None
        self.env = None
        self.relay_policy = relay_policy(args)
        self.relay_config = None
        self.listeners = {}
        self.metadata = dict(run_id=run_id, mode=args.mode, profile=args.profile,
                             review=dry_run(args, profile, run_id))
        self.gates = {}
        self.counts = collections.Counter()
        self.failed_frames = collections.deque(maxlen=32)
        self.active_transfers = {}
        self.diagnostic_background = {}
        self.mixed_case = None
        self.mixed_probe_stats = {}
        self.mixed_probe_events = collections.deque(maxlen=2000)
        self.mixed_peak_active = 0
        self.resource_coverage = {}
        self.probe_pause_requested = asyncio.Event()
        self.probe_paused = asyncio.Event()
        self.probe_resume = asyncio.Event()
        self.probe_resume.set()
        self.probe_connections = 0
        self.admin_bulk_next = 0
        self.admin_bulk_active = None
        self.admin_bulk_results = []
        self.admin_bulk_peak_active = 0
        self.latencies = collections.deque(maxlen=10000)
        self.identities = {}
        self.sequences = collections.Counter()
        self.confirmed = {}
        self.live_streams = {}
        self.epochs = dict.fromkeys(SERVICES, 0)
        self.fault_services = set()
        self.fault_active = False
        self.fault_generation = 0
        self.tasks = []
        self.heartbeat_task = None
        self.stop_workers = asyncio.Event()
        self.started = time.monotonic()
        self.work_started = None
        self.work_deadline = None
        self.load_time = 0
        self.recovery_time = 0
        self.load_intervals = []
        self.load_interval_started = None
        self.cleanup_status = {"controller": "unverified", "target": "unverified"}
        self.collected = False
        self.runtime_removed = False
        if args.profile == "dev" and args.mode == "soak":
            self.gate("real_365s_quiet", "not_exercised", reason="dev profile uses an 8-second gate")
            self.gate("default_300s_expiry", "not_exercised", reason="dev profile uses a 2-second policy")

    def event(self, event, **fields):
        self.artifacts.event("timeline.jsonl", dict(event=event, **fields), cap=10000)

    def gate(self, name, status, **fields):
        self.gates[name] = dict(status=status, **fields)
        self.event("gate", gate=name, **self.gates[name])

    async def heartbeat(self):
        while True:
            before = time.time()
            result = await self.control.rpc("heartbeat", timeout=15)
            after = time.time()
            self.metadata["clock"] = dict(target_minus_controller_seconds=result["utc"] - (before + after) / 2,
                                          uncertainty_seconds=(after - before) / 2,
                                          note="UTC correlation only; deadlines use local monotonic clocks")
            await asyncio.sleep(5)

    async def setup(self):
        if getattr(self.args, "workload", "capacity") == "supported-admin":
            require(self.args.binary_profile == "release", "supported-admin requires the official release profile, not a candidate")
        self.metadata["controller_capacity"] = capacity(self.root, self.args.fd_soft, self.args.fd_hard)
        source = Path(self.args.controller_binary).expanduser().resolve()
        require(source.is_file(), "controller binary is missing")
        shutil.copyfile(source, self.root / "locho")
        (self.root / "locho").chmod(0o700)
        self.env = await environment(self.root, self.args.controller_relay_env, self.artifacts)
        self.relay_config = write_relay_config(self.root, self.relay_policy, self.env)
        self.metadata.update(relay_policy=self.relay_policy,
                             relay_token_available=bool(self.relay_policy and self.env.get(self.relay_policy["token_env"])))
        status, version = await capture(str(self.root / "locho"), "--version", env=self.env, timeout=5)
        require(status == 0, "controller binary version probe failed")
        controller = dict(binary=binary_metadata(self.root / "locho", self.args.controller_provenance,
                                               self.args.controller_source_commit)
                          | {"version": version.decode().strip()}, os=platform.platform(),
                          architecture=platform.machine(), python=platform.python_version())
        await self.control.start()
        self.metadata["target_runtime"] = self.control.root
        self.artifacts.runtime_paths.add(self.control.root)
        reply = await self.control.rpc("initialize", dict(fd_soft=self.args.fd_soft, fd_hard=self.args.fd_hard,
                                     relay_env=self.args.target_relay_env, provenance=self.args.target_provenance,
                                     commit=self.args.target_source_commit, dev_idle=2, relay_policy=self.relay_policy,
                                     fixture_observation=getattr(self.args, "fixture_observation", False) or (
                                          self.args.mode == "diagnostic" and getattr(self.args, "diagnostic_kind", "fragmentation") in ("duplex", "mixed", "staggered", "transport")),
                                      resource_observation=self.args.mode == "diagnostic" and
                                          getattr(self.args, "diagnostic_kind", "fragmentation") in ("staggered", "transport")), timeout=90)
        self.metadata.update(controller=controller, target=reply["metadata"])
        if self.args.binary_profile in ("baseline", "release"):
            require(release_version(controller["binary"]["version"]) == "1.3.0"
                    and release_version(reply["metadata"]["binary"]["version"]) == "1.3.0",
                    "baseline requires 1.3.0 at both ends")
        if self.args.binary_profile == "baseline":
            require(self.args.controller_provenance == "modified" and self.args.target_provenance == "official",
                    "baseline requires explicitly declared modified controller and official target")
        if self.args.binary_profile == "release":
            require(self.args.controller_provenance == self.args.target_provenance == "official",
                    "release comparison requires official binaries at both ends")
            self.metadata["baseline_note"] = "Official-release comparison; patched DNS-recovery baseline not exercised"
        self.heartbeat_task = asyncio.create_task(self.heartbeat())
        self.listeners = dict(tt=tcp_port(), tu=tcp_port(), ut=self.root / "a", uu=self.root / "b")
        require(self.listeners["tt"] != self.listeners["tu"], "controller listener ports collided")
        configs = [f'host_id = {quote(reply["host_id"])}\nlisten_host = "127.0.0.1"\n']
        for name in (*SERVICES, "default", "shortidle"):
            capability = reply["capabilities"][name]
            self.artifacts.secrets.update((capability, capability.split(":")[-1]))
            listener = self.listeners.get(name, self.root / ("d" if name == "default" else "i"))
            field = f"listen_unix = {quote(listener)}" if isinstance(listener, Path) else f"listen_port = {listener}"
            idle = "" if name == "default" else f'tcp_idle_timeout_secs = {2 if name == "shortidle" else 0}\n'
            configs.append(f'[[services]]\ncapability = {quote(capability)}\n{field}\n{idle}')
        write_toml(self.root / "attach.toml", "\n".join(configs))
        self.attachment = OwnedProcess("attachment", [self.root / "locho", "attach", "--config",
                                                     self.root / "attach.toml", *relay_arguments(self.relay_config)], self.artifacts,
                                       self.env, self.args.fd_soft, self.args.fd_hard,
                                        budget=self.profile["outer_budget"] - (time.monotonic() - self.started))
        self.attachment.resource_observation = self.args.mode == "diagnostic" and getattr(self.args, "diagnostic_kind", "fragmentation") in ("staggered", "transport")
        await self.attachment.start()
        await self.probe_all()
        # Warm resource baseline: start discovery, fixtures and checked traffic first.
        await asyncio.sleep(1)
        self.baseline = (await self.stabilized_fd_baseline() if getattr(self.args, "workload", "capacity") == "supported-admin"
                         else await self.sample())
        self.metadata["endpoints"] = dict(controller={k: str(v) for k, v in self.listeners.items()},
                                           target=reply["metadata"]["fixtures"])
        self.artifacts.json("metadata.json", self.metadata)
        self.gate("readiness", "passed")

    def identity(self, name, worker):
        key = f"{name}:{worker}"
        identity = self.identities.setdefault(key, uuid.uuid4().bytes)
        self.sequences[key] += 1
        return identity, self.sequences[key]

    async def probe_all(self):
        deadline = min(time.monotonic() + RECOVERY, self.work_deadline or float("inf"))
        async def probe(name):
            try:
                async with asyncio.timeout(5):
                    reader, writer = await connect(self.listeners[name])
                    try:
                        # A timed-out probe may arrive after the next connection
                        # once discovery finishes. Fresh probes get fresh IDs, so
                        # their arrival ordering is not mistaken for worker replay.
                        self.counts["probe_attempts"] += 1
                        identity, seq = self.identity(name, f"probe-{self.counts['probe_attempts']}")
                        await exchange(reader, writer, identity, seq, 256, half_close=True)
                        self.confirmed[identity.hex()] = (name, seq)
                        self.counts["probe_success"] += 1
                    finally:
                        await close(writer)
            except BaseException as error:
                if not isinstance(error, asyncio.CancelledError):
                    self.counts["probe_failures"] += 1
                    self.artifacts.event("operations.jsonl", dict(combination=name, worker="probe",
                                         outcome="failed", error_type=type(error).__name__))
                raise
        async with asyncio.timeout_at(deadline):
            while True:
                results = await asyncio.gather(*(probe(name) for name in SERVICES), return_exceptions=True)
                for result in results:
                    if isinstance(result, BaseException) and not isinstance(
                            result, (OSError, asyncio.TimeoutError, asyncio.IncompleteReadError)):
                        raise result
                if all(result is None for result in results):
                    return
                await asyncio.sleep(0.2)

    async def sample(self):
        self.capture_active_transfers("resource_sample")
        if self.heartbeat_task and self.heartbeat_task.done():
            self.heartbeat_task.result()
        local = await self.attachment.sample()
        self.artifacts.event("metrics-controller.jsonl", local, cap=20000)
        remote = await self.control.rpc("sample", timeout=15)
        if self.args.mode == "diagnostic" and getattr(self.args, "diagnostic_kind", "fragmentation") in ("staggered", "transport"):
            for record in [local, *remote["records"].values()]:
                observed = record.get("resource_observation", {})
                coverage = self.resource_coverage.setdefault(record["process"], dict(cpu_intervals=0, cgroup_samples=0,
                    socket_samples=0, unavailable=[]))
                coverage["cpu_intervals"] += "interval_cpu_percent" in observed
                coverage["cgroup_samples"] += any("cpu.stat" in entry for entry in observed.get("cgroups", []))
                coverage["socket_samples"] += bool(observed.get("loopback_tcp_sockets"))
                coverage["unavailable"] = sorted(set(coverage["unavailable"]) | set(observed.get("unavailable", [])))
            self.metadata["resource_observation_coverage"] = self.resource_coverage
        for record in remote["records"].values():
            self.artifacts.event("metrics-target.jsonl", record, cap=20000)
        require(all(s["errors"] == 0 and s["duplicates"] == 0 for s in remote["fixtures"].values()),
                "target fixture corruption or replay")
        for identity, (name, _) in self.confirmed.items():
            expected = "unix" if name.endswith("u") else name
            require(all(identity not in state["highwater"] for backend, state in remote["fixtures"].items()
                        if backend != expected), "checked operation reached wrong backend fixture")
        self.metadata["fixture_counters"] = {k: {field: value for field, value in s.items() if field != "highwater"}
                                              for k, s in remote["fixtures"].items()}
        self.metadata["transport_observations"] = dict(controller=local["transport"],
                                                       target=remote["records"]["host"]["transport"])
        self.metadata["transport_history_controller"] = list(self.attachment.paths)
        self.metadata["fd_peaks"] = dict(controller=local["fd_peak"], target=remote["records"]["host"]["fd_peak"])
        self.metadata["observed_socket_endpoints"] = dict(controller=local["endpoints"],
                                                         target=remote["records"]["host"]["endpoints"])
        return dict(attachment=local, host=remote["records"]["host"])

    def capture_active_transfers(self, reason):
        records = []
        for active in list(self.active_transfers.values()):
            record = {key: value for key, value in active.items() if key != "trace"}
            record.update(reason=reason, current_fault_epoch=self.fault_generation,
                          telemetry=active["trace"].snapshot())
            self.artifacts.event("transfer-progress.jsonl", record)
            records.append(record)
        return records

    async def stabilized_fd_baseline(self):
        """Reject transient startup peaks instead of silently buying leak headroom."""
        began = time.monotonic()
        first = previous = None
        consecutive = 0
        review = dict(status="sampling", samples=[], consecutive_required=3, minimum_seconds=3,
                      sample_pause_seconds=1, maximum_samples=10, budget_seconds=15)
        self.metadata["warmed_fd_baseline"] = review
        try:
            async with asyncio.timeout(15):
                for _ in range(10):
                    current = await self.sample()
                    require(set(current) == {"attachment", "host"}, "both baseline process samples required")
                    if first is None:
                        first = current
                    for name, record in current.items():
                        require(all(record[key] == first[name][key] for key in ("pid", "generation", "start_identity")),
                                "process lifetime changed while stabilizing baseline")
                        require(type(record.get("fds")) is int and record["fds"] >= 3, "baseline FD counts unavailable")
                    counts = {name: record["fds"] for name, record in current.items()}
                    consecutive = consecutive + 1 if counts == previous else 1
                    elapsed = time.monotonic() - began
                    review["samples"].append(dict(elapsed_seconds=elapsed, fds=counts,
                                                  consecutive_equal_samples=consecutive))
                    if consecutive >= 3 and elapsed >= 3:
                        review.update(status="passed", selected=current, elapsed_seconds=elapsed)
                        self.gate("warmed_fd_baseline", "passed", fds=counts, samples=len(review["samples"]),
                                  elapsed_seconds=elapsed, consecutive_equal_samples=consecutive)
                        return current
                    previous = counts
                    await asyncio.sleep(1)
            raise AssertionError("warmed FD baseline did not stabilize within ten samples")
        except BaseException as error:
            review.update(status="failed", error_type=type(error).__name__)
            self.gate("warmed_fd_baseline", "failed", error_type=type(error).__name__, samples=len(review["samples"]))
            raise

    def admin_payload(self, name, sequence):
        """Claim at most one scheduled bulk exchange atomically on the event loop.

        No worker waits on a bulk permit, no successful operation is delayed to
        manufacture overlap, and a missed/failed slot is never replayed.
        """
        small = (1, 256, 16384)[sequence % 3]
        slots = self.profile.get("bulk_slots", [])
        if self.work_started is None or self.admin_bulk_active is not None or self.admin_bulk_next >= len(slots):
            return small, None
        elapsed = time.monotonic() - self.work_started
        slot = slots[self.admin_bulk_next]
        if elapsed < slot["scheduled_offset_seconds"] or name != slot["combination"]:
            return small, None
        self.admin_bulk_next += 1
        record = dict(slot, actual_offset_seconds=elapsed, outcome="inflight")
        self.admin_bulk_results.append(record)
        if elapsed >= slot["scheduled_offset_seconds"] + 60:
            record["outcome"] = "missed"
            self.event("supported_bulk_missed", **record)
            return small, None
        self.admin_bulk_active = record
        self.admin_bulk_peak_active = max(self.admin_bulk_peak_active, 1)
        self.event("supported_bulk_start", **record)
        return 1048576, record

    def admin_bulk_gate(self):
        expected = self.profile["expected_bulk_operations"]
        passed = (len(self.admin_bulk_results) == expected and self.admin_bulk_next == expected
                  and self.admin_bulk_active is None and all(record["outcome"] == "success" for record in self.admin_bulk_results))
        self.metadata["supported_bulk_operations"] = self.admin_bulk_results
        self.gate("supported_bulk_coverage", "passed" if passed else "failed", expected=expected,
                  successful=sum(record["outcome"] == "success" for record in self.admin_bulk_results),
                  observed_peak_active=self.admin_bulk_peak_active, configured_max_active=1,
                  note="missing, missed, interrupted or failed slots never satisfy bulk coverage")
        require(passed, "supported-admin bulk slots did not all complete successfully")

    async def worker(self, name, index, *, small_only=False, stopped=None, fixed_size=None, probe=False):
        reader = writer = None
        global_stop = self.stop_workers
        operation_inflight = False
        connection_epoch = self.epochs[name]
        try:
            while not global_stop.is_set() and not (stopped and stopped.is_set()):
                if probe and self.probe_pause_requested.is_set():
                    self.probe_paused.set()
                    await self.probe_resume.wait()
                    self.probe_paused.clear()
                began = time.monotonic()
                probe_started_utc = time.time() if probe else None
                probe_case = self.mixed_case if probe else None
                epoch = self.epochs[name]
                active = self.fault_active and name in self.fault_services
                identity, sequence = self.identity(name, f"worker-{index}")
                operation_inflight = True
                admin_bulk = None
                if getattr(self.args, "workload", "capacity") == "supported-admin" and fixed_size is None and not small_only:
                    size, admin_bulk = self.admin_payload(name, sequence)
                else:
                    size = fixed_size if fixed_size is not None else (4096 if small_only else (1024 * 1024 if sequence % 20 == 0 else (1, 256, 16384)[sequence % 3]))
                trace = TransferProgress(size) if (getattr(self.args, "transfer_telemetry", False)
                        or getattr(self.args, "fixture_observation", False) or self.args.mode == "diagnostic") else None
                transfer_key = ("worker", name, index)
                if trace and size >= 1024 * 1024:
                    self.active_transfers[transfer_key] = dict(combination=name, worker=index,
                        identity=identity.hex(), sequence=sequence, operation_epoch=epoch,
                        attachment_generation=self.attachment.generation if self.attachment else None, trace=trace)
                try:
                    async with asyncio.timeout(10):
                        if writer is None:
                            connection_epoch = self.epochs[name]
                            connect_started = time.monotonic()
                            if trace:
                                trace.socket_phase = "connect_wait"
                            reader, writer = await connect(self.listeners[name])
                            if probe:
                                self.probe_connections += 1
                            if trace:
                                trace.times["connect"] += time.monotonic() - connect_started
                            self.live_streams[name, index] = connection_epoch
                        operation_reader, operation_writer = trace.bind(reader, writer) if trace else (reader, writer)
                        duplex = not small_only and admin_bulk is None and sequence % 40 == 0
                        half = not small_only and admin_bulk is None and sequence % 11 == 0
                        if duplex:
                            await duplex_exchange(operation_reader, operation_writer, identity, sequence, size)
                        else:
                            await exchange(operation_reader, operation_writer, identity, sequence, size,
                                            fragmented=not small_only and admin_bulk is None and sequence % 7 == 0,
                                            slow=not small_only and admin_bulk is None and sequence % 13 == 0, half_close=half)
                        if trace:
                            trace.frame_verified = True
                        operation_inflight = False
                        self.counts["success"] += 1
                        self.confirmed[identity.hex()] = (name, sequence)
                        self.counts[f"{name}.success"] += 1
                        self.counts["bytes"] += size * 2
                        latency = time.monotonic() - began
                        self.latencies.append(latency)
                        if probe:
                            self.counts["diagnostic_probe_success"] += 1
                            stats = self.mixed_probe_stats.setdefault(probe_case or "between_cases", dict(success=0, max_latency_seconds=0))
                            stats["success"] += 1
                            stats["max_latency_seconds"] = max(stats["max_latency_seconds"], latency)
                            self.mixed_probe_events.append(dict(started_utc=probe_started_utc, finished_utc=time.time(),
                                                                finished_monotonic=time.monotonic(),
                                                                latency_seconds=latency))
                            self.artifacts.event("operations.jsonl", dict(combination=name, worker=index,
                                identity=identity.hex(), sequence=sequence, outcome="success", case=probe_case,
                                probe=True, size=size, latency=latency, telemetry=trace.snapshot() if trace else None))
                        if sequence % 100 == 0 or trace and size >= 1024 * 1024 or admin_bulk is not None:
                            self.artifacts.event("operations.jsonl", dict(combination=name, worker=index,
                                                 identity=identity.hex(), sequence=sequence, outcome="success",
                                                  latency=latency, size=size, supported_bulk=admin_bulk is not None,
                                                  telemetry=trace.snapshot() if trace else None))
                        # Pacing bounds churn/TIME_WAIT pressure and outstanding bytes.
                        churn = not probe and index % 3 == 2 and sequence % 4 == 0
                        if half or duplex or churn:
                            if not (half or duplex):
                                operation_writer.write_eof()
                                await operation_writer.drain()
                                require(await operation_reader.read(1) == b"", "churn stream returned extra bytes")
                            await close(operation_writer)
                            reader = writer = None
                            self.live_streams.pop((name, index), None)
                except (OSError, asyncio.TimeoutError, asyncio.IncompleteReadError) as error:
                    expected = fault_failure(self.fault_active and name in self.fault_services, active,
                                             epoch, connection_epoch, self.epochs[name])
                    self.counts["fault_failures" if expected else "unexpected_failures"] += 1
                    failure = dict(combination=name, worker=index,
                                         identity=identity.hex(), sequence=sequence, outcome="failed", expected=expected,
                                         operation_epoch=epoch, connection_epoch=connection_epoch,
                                         fault_epoch=self.epochs[name], error_type=type(error).__name__,
                                          telemetry=trace.snapshot() if trace else None)
                    if probe:
                        failure.update(probe=True, case=probe_case)
                    self.artifacts.event("operations.jsonl", failure)
                    if not expected:
                        self.failed_frames.append(failure)
                    if writer:
                        await close(writer)
                    reader = writer = None
                    self.live_streams.pop((name, index), None)
                    require(expected, "unexpected steady-state I/O failure")
                except asyncio.CancelledError:
                    if operation_inflight:
                        self.counts["aborted_operations"] += 1
                        self.artifacts.event("operations.jsonl", dict(combination=name, worker=index,
                                             identity=identity.hex(), sequence=sequence, outcome="aborted",
                                             reason="harness cancellation; operation is not replayed",
                                             telemetry=trace.snapshot() if trace else None))
                    raise
                except AssertionError:
                    self.counts["validation_failures"] += 1
                    self.artifacts.event("operations.jsonl", dict(combination=name, worker=index,
                                         identity=identity.hex(), sequence=sequence, outcome="validation_failed",
                                         telemetry=trace.snapshot() if trace else None))
                    raise
                finally:
                    self.active_transfers.pop(transfer_key, None)
                    if admin_bulk is not None:
                        admin_bulk.update(outcome="success" if not operation_inflight else "incomplete",
                                          finished_offset_seconds=time.monotonic() - self.work_started)
                        self.admin_bulk_active = None
                        self.event("supported_bulk_end", **admin_bulk)
                await asyncio.sleep(self.args.pace)
        finally:
            self.live_streams.pop((name, index), None)
            if writer and not operation_inflight:
                try:
                    async with asyncio.timeout(5):
                        writer.write_eof()
                        await writer.drain()
                        require(await reader.read(1) == b"", "worker drain returned extra bytes")
                except (OSError, asyncio.TimeoutError):
                    self.counts["drain_failures"] += 1
                except AssertionError:
                    self.counts["validation_failures"] += 1
                    raise
                finally:
                    await close(writer)
            elif writer:
                # Aborting mid-response can leave legitimate frame bytes unread.
                # They are not a fresh EOF/validation gate and must not inflate
                # corruption counters while cleaning up an already failed run.
                await close(writer)

    def start_workers(self, total):
        self.load_interval_started = time.monotonic()
        self.stop_workers = asyncio.Event()
        self.tasks = [asyncio.create_task(self.worker(name, index))
                      for name, count in distribution(total).items() for index in range(count)]
        self.event("load_level", total_concurrency=total, distribution=distribution(total))

    async def drain(self):
        if self.load_interval_started is not None:
            self.load_intervals.append(time.monotonic() - self.load_interval_started)
            self.load_interval_started = None
        self.stop_workers.set()
        if self.tasks:
            results = await asyncio.wait_for(asyncio.gather(*self.tasks, return_exceptions=True), 15)
            for result in results:
                if isinstance(result, BaseException):
                    raise result
        self.tasks = []

    async def quiet(self):
        connections = []
        real = self.args.profile == "acceptance"
        quiet_name = "real_365s_quiet" if real else "dev_quiet"
        expiry_name = "default_300s_expiry" if real else "dev_short_expiry"
        try:
            for name in SERVICES:
                reader, writer = await connect(self.listeners[name])
                connections.append((reader, writer, uuid.uuid4().bytes))
                await asyncio.wait_for(exchange(reader, writer, connections[-1][2], 1, 1), 10)
            reader, writer = await connect(self.root / ("d" if real else "i"))
            connections.append((reader, writer, uuid.uuid4().bytes))
            await asyncio.wait_for(exchange(reader, writer, connections[-1][2], 1, 1), 10)

            async def silent(reader, writer, identity):
                try:
                    async with asyncio.timeout(self.profile["quiet_seconds"]):
                        await reader.read(1)
                        raise AssertionError("quiet stream closed or received application bytes")
                except asyncio.TimeoutError:
                    pass
                await asyncio.wait_for(exchange(reader, writer, identity, 2, 256), 10)

            async def expiry():
                began = time.monotonic()
                async with asyncio.timeout(320 if real else 5):
                    try:
                        require(await reader.read(1) == b"", "idle-expiry stream returned data")
                    except ConnectionResetError:
                        pass
                elapsed = time.monotonic() - began
                require((290 <= elapsed <= 320) if real else (1 <= elapsed <= 5), "idle expiry outside gate window")
                self.gate(expiry_name, "passed", elapsed=elapsed)

            await asyncio.gather(expiry(), *(silent(*c) for c in connections[:-1]))
            self.gate(quiet_name, "passed", quiet_seconds=self.profile["quiet_seconds"])
        except BaseException:
            if quiet_name not in self.gates:
                self.gate(quiet_name, "failed")
            if expiry_name not in self.gates:
                self.gate(expiry_name, "failed")
            raise
        finally:
            for _, writer, _ in connections:
                await close(writer)

    async def resource_gate(self):
        await self.drain()
        await asyncio.sleep(2)
        records = await self.sample()
        for name in self.baseline:
            growth = same_lifetime_growth(self.baseline[name], records[name], self.args.max_fd_growth)
            self.gate(f"same_lifetime_fd_{name}", "passed", growth=growth, allowance=self.args.max_fd_growth,
                      pid=records[name]["pid"], generation=records[name]["generation"])

    async def fault(self, scope):
        started = time.monotonic()
        require(self.attachment.child.returncode is None, "unexpected attachment exit before scheduled fault")
        self.fault_active = True
        self.fault_services = set(SERVICES)
        self.fault_generation += 1
        for name in self.fault_services:
            self.epochs[name] = self.fault_generation
        listeners = {name: (os.stat(path).st_dev, os.stat(path).st_ino)
                     for name, path in self.listeners.items() if isinstance(path, Path)}
        pid = self.attachment.pid
        self.event("fault_start", scope=scope, operation_epochs=dict(self.epochs),
                   generation=self.fault_generation,
                   active_large_operations=self.capture_active_transfers("fault_start"),
                   expected_interrupted_streams={name: sum(service == name for service, _ in self.live_streams)
                                                for name in self.fault_services})
        async with asyncio.timeout_at(min(started + RECOVERY, self.work_deadline)):
            if scope == "attachment":
                await self.attachment.stop()
                require(self.attachment.child.returncode == 0, "scheduled attachment stop failed")
                await self.attachment.start()
            else:
                await self.control.rpc("restart", dict(process=scope), timeout=RECOVERY)
                require(self.attachment.pid == pid, "attachment changed across target recovery")
                require(all((os.stat(self.listeners[name]).st_dev, os.stat(self.listeners[name]).st_ino) == identity
                            for name, identity in listeners.items()), "attachment listener identity changed")
            await self.probe_all()
        self.fault_active = False
        elapsed = time.monotonic() - started
        self.recovery_time += elapsed
        self.event("fault_end", scope=scope, recovery_seconds=elapsed, first_fresh_success=time.time(),
                   generation=self.fault_generation, operation_epochs=dict(self.epochs),
                   active_large_operations=self.capture_active_transfers("fault_end"))
        self.gate(f"recovery_{scope}", "passed", duration=elapsed)

    def transport_gates(self):
        paths = self.metadata.get("transport_history_controller", [])
        require(any("relay(" in record["path"] or "direct(" in record["path"] for record in paths),
                "actual transport path was not observed")
        self.gate("transport_observation", "passed")
        if self.relay_policy:
            observed = custom_relay_observed(paths, self.relay_policy)
            self.gate("custom_relay_observation", "passed" if observed else "not_exercised",
                      requested=self.relay_policy["url"], scope="observed path, not proof that every byte was relayed")
            require(observed, "custom relay path was not observed; no custom-relay coverage claim")

    async def smoke(self):
        self.work_started = time.monotonic()
        self.work_deadline = self.work_started + self.profile["duration"]
        try:
            async with asyncio.timeout_at(self.work_deadline):
                for name, size in [*( (name, 256) for name in SERVICES), ("tt", 1024 * 1024)]:
                    began = time.monotonic()
                    reader, writer = await connect(self.listeners[name])
                    try:
                        identity, sequence = self.identity(name, "smoke")
                        async with asyncio.timeout(10):
                            await exchange(reader, writer, identity, sequence, size, half_close=True)
                        self.confirmed[identity.hex()] = (name, sequence)
                        self.counts["success"] += 1
                        self.counts[f"{name}.success"] += 1
                        self.counts["bytes"] += size * 2
                        self.latencies.append(time.monotonic() - began)
                        self.load_time += time.monotonic() - began
                    finally:
                        await close(writer)
                await self.resource_gate()
                self.gate("integrity_and_steady_state", "passed")
                self.gate("forwarding_coverage", "passed", combinations=list(SERVICES))
                self.gate("required_metrics", "passed")
                self.transport_gates()
        finally:
            self.metadata["workload_seconds"] = time.monotonic() - self.work_started

    def start_diagnostic_background(self):
        self.load_interval_started = time.monotonic()
        self.stop_workers = asyncio.Event()
        workers = [("tt", 0), ("tt", 1), ("tu", 0), ("tu", 1), ("tu", 2), ("ut", 0), ("uu", 0)]
        self.tasks = [asyncio.create_task(self.worker(name, index, small_only=True)) for name, index in workers]
        self.event("diagnostic_background", workers=workers, payload_bytes=4096, pace=self.args.pace)

    def check_background(self):
        for task in self.tasks:
            if task.done():
                task.result()
                raise AssertionError("diagnostic background worker exited early")

    async def diagnostic_transfer(self, label, slot, name, fragmented, duplex, barrier=None, start_offset=None):
        key = f"diagnostic-{label}-{slot}"
        identity = None
        reader = writer = None
        trace = TransferProgress(256)
        trace.socket_phase = "connect_wait"
        stage = "warmup"
        record = dict(case=label, combination=name, worker=key, fragmented=fragmented, duplex=duplex,
                      attachment_generation=self.attachment.generation, operation_epoch=self.epochs[name],
                      connection_epoch=self.epochs[name], operation_timeout=10, large_transfer_exercised=False)
        try:
            async with asyncio.timeout(10):
                reader, writer = await connect(self.listeners[name])
                self.live_streams[name, key] = self.epochs[name]
                # Match the failed operation's established-stream context without
                # replaying its identity, sequence, or interrupted request.
                for _ in range(3):
                    trace = TransferProgress(256)
                    observed_reader, observed_writer = trace.bind(reader, writer)
                    identity, sequence = self.identity(name, key)
                    await exchange(observed_reader, observed_writer, identity, sequence, 256)
                    trace.frame_verified = True
                    self.confirmed[identity.hex()] = (name, sequence)
                    self.counts["success"] += 1
                    self.counts[f"{name}.success"] += 1
                    self.counts["bytes"] += 512
                    self.counts["diagnostic_warmup_success"] += 1
                if barrier:
                    epoch = await barrier.wait()
            if start_offset is not None:
                # Waiting is outside the operation deadline; each operation gets
                # the same unchanged 10s bound beginning at its actual start.
                stage = "scheduled_start"
                await asyncio.sleep(max(0, epoch + start_offset - time.monotonic()))
                actual = time.monotonic()
                record.update(round_epoch_monotonic=epoch, planned_start_offset_seconds=start_offset,
                              actual_start_monotonic=actual, actual_start_offset_seconds=actual - epoch,
                              start_lateness_seconds=actual - epoch - start_offset)
            stage = "large_transfer"
            record["large_transfer_exercised"] = True
            trace = TransferProgress(1024 * 1024)
            record["large_started_controller_utc"] = time.time()
            record["large_started_controller_monotonic"] = time.monotonic()
            observed_reader, observed_writer = trace.bind(reader, writer)
            identity, sequence = self.identity(name, key)
            record.update(identity=identity.hex(), sequence=sequence)
            self.active_transfers[label, slot] = dict(case=label, combination=name, worker=key,
                                                      identity=identity.hex(), sequence=sequence, duplex=duplex, trace=trace)
            if getattr(self.args, "diagnostic_kind", "fragmentation") in ("mixed", "staggered", "transport"):
                self.mixed_peak_active = max(self.mixed_peak_active, len(self.active_transfers))
                self.capture_active_transfers("mixed_large_start")
            async with asyncio.timeout(10):
                if duplex:
                    await duplex_exchange(observed_reader, observed_writer, identity, sequence, 1024 * 1024)
                else:
                    await exchange(observed_reader, observed_writer, identity, sequence, 1024 * 1024,
                                   fragmented=fragmented)
                trace.frame_verified = True
                self.confirmed[identity.hex()] = (name, sequence)
                self.counts["success"] += 1
                self.counts[f"{name}.success"] += 1
                self.counts["bytes"] += 2 * 1024 * 1024
                self.latencies.append(time.monotonic() - trace.started)
                if not duplex:
                    observed_writer.write_eof()
                    await observed_writer.drain()
                    require(await observed_reader.read(1) == b"", "diagnostic churn returned extra bytes")
                await close(observed_writer)
            record.update(outcome="success", stage=stage, telemetry=trace.snapshot())
        except (OSError, asyncio.TimeoutError, asyncio.IncompleteReadError) as error:
            self.counts["unexpected_failures"] += 1
            record.update(outcome="failed", stage=stage, error_type=type(error).__name__, telemetry=trace.snapshot())
        except AssertionError:
            self.counts["validation_failures"] += 1
            record.update(outcome="validation_failed", stage=stage, telemetry=trace.snapshot())
            self.artifacts.event("operations.jsonl", record)
            raise
        except asyncio.CancelledError:
            self.counts["aborted_operations"] += 1
            record.update(outcome="aborted", stage=stage, telemetry=trace.snapshot())
            self.artifacts.event("operations.jsonl", record)
            raise
        finally:
            if record["large_transfer_exercised"]:
                record["large_finished_controller_utc"] = time.time()
                record["large_finished_controller_monotonic"] = time.monotonic()
            self.active_transfers.pop((label, slot), None)
            self.live_streams.pop((name, key), None)
            if writer:
                await close(writer)
        if identity is not None:
            record.setdefault("identity", identity.hex())
        self.artifacts.event("operations.jsonl", record)
        return record

    async def diagnostic_case(self, label, fragmented=False, duplex_count=None, mixed_duplex_count=None, offsets=None):
        if mixed_duplex_count is not None:
            flows = [(name, False, duplex) for name, _, duplex in mixed_flows(mixed_duplex_count)]
        elif duplex_count:
            flows = [(name, False, True) for name in ("ut", "uu", "tu")[:duplex_count]]
        else:
            flows = [("tt", fragmented, False), ("ut", fragmented, False), ("uu", fragmented, False),
                     ("ut", False, True), ("uu", False, True)]
        self.event("diagnostic_case_start", case=label, primary_fragmented=fragmented,
                   background_workers=12 - len(flows), large_streams=len(flows), flows=flows,
                    synchronized_large_start=offsets is None or not any(offsets), planned_start_offsets_seconds=offsets,
                    total_distribution=distribution(12))
        barrier = asyncio.Barrier(len(flows)) if duplex_count or mixed_duplex_count is not None else None
        if offsets is not None:
            require(len(offsets) == len(flows), "one start offset required per foreground flow")
            barrier = RoundStarts(len(flows))
        if mixed_duplex_count is not None:
            self.mixed_case = label
            self.mixed_peak_active = 0
        tasks = [asyncio.create_task(self.diagnostic_transfer(label, index, *flow, barrier,
                    **({"start_offset": offsets[index]} if offsets is not None else {})))
                 for index, flow in enumerate(flows)]
        sampler_stop = asyncio.Event()
        sampler = asyncio.create_task(self.diagnostic_sampling(sampler_stop))
        try:
            records = await asyncio.gather(*tasks)
            if sampler.done():
                sampler.result()
            self.check_background()
            # This is a later snapshot, not proof of target receipt by the client
            # deadline. Only test-generated frame identities are retained.
            await asyncio.sleep(2)
            remote = await self.control.rpc("sample", timeout=15)
            captured = time.time()
            for record in records:
                backend = "unix" if record["combination"].endswith("u") else record["combination"]
                record["target_frame_snapshot"] = dict(captured_controller_utc=captured,
                    last_validated_sequence=remote["fixtures"][backend]["highwater"].get(record.get("identity")),
                    scope="eventual target snapshot after closure/settle; not receipt timing at the client deadline")
            require(all(state["errors"] == 0 and state["duplicates"] == 0 for state in remote["fixtures"].values()),
                    "diagnostic fixture corruption or replay")
            result = dict(case=label, passed=all(record["outcome"] == "success" for record in records), flows=records)
            if mixed_duplex_count is not None:
                started = [record["large_started_controller_utc"] for record in records
                           if "large_started_controller_utc" in record]
                finished = [record["large_finished_controller_utc"] for record in records
                            if "large_finished_controller_utc" in record]
                monotonic_starts = [record["large_started_controller_monotonic"] for record in records
                                    if "large_started_controller_monotonic" in record]
                monotonic_ends = [record["large_finished_controller_monotonic"] for record in records
                                  if "large_finished_controller_monotonic" in record]
                during_large = ([event for event in self.mixed_probe_events
                                 if min(monotonic_starts) <= event["finished_monotonic"] <= max(monotonic_ends)]
                                if monotonic_starts and monotonic_ends else [])
                result.update(ordinary_large_streams=4, duplex_large_streams=mixed_duplex_count,
                              planned_start_offsets_seconds=offsets,
                              observed_peak_large_operations=self.mixed_peak_active,
                              large_start_spread_seconds=max(started) - min(started) if started else None,
                              probe=dict(self.mixed_probe_stats.get(label, {})) | {
                                  "success_during_large_window": len(during_large),
                                  "max_latency_during_large_seconds": max(
                                      (event["latency_seconds"] for event in during_large), default=None)})
            self.metadata.setdefault("diagnostic_cases", []).append(result)
            self.event("diagnostic_case_end", case=label, passed=result["passed"])
            return result
        finally:
            if mixed_duplex_count is not None:
                self.mixed_case = None
            for task in tasks:
                if not task.done():
                    task.cancel()
            sampler_stop.set()
            # Do not cancel a healthy in-flight control RPC and poison its
            # request/response stream merely because this case has finished.
            try:
                await asyncio.wait_for(sampler, 15)
            finally:
                await asyncio.gather(*tasks, return_exceptions=True)

    async def diagnostic_sampling(self, stopped):
        while not stopped.is_set():
            self.check_background()
            await self.sample()
            try:
                await asyncio.wait_for(stopped.wait(), 1)
            except asyncio.TimeoutError:
                pass

    async def diagnostic(self):
        if getattr(self.args, "diagnostic_kind", "fragmentation") in ("mixed", "staggered", "transport"):
            return await self.mixed_diagnostic()
        if getattr(self.args, "diagnostic_kind", "fragmentation") == "duplex":
            return await self.duplex_diagnostic()
        self.work_started = time.monotonic()
        self.work_deadline = self.work_started + self.profile["duration"]
        self.metadata["diagnostic_scope"] = "4 independent fresh-ID cases; 3 ordinary large flows + 2 duplex flows + 7 small workers; not a soak certification"
        self.start_diagnostic_background()
        try:
            async with asyncio.timeout_at(self.work_deadline):
                await self.diagnostic_case("before_unfragmented", False)
                await self.diagnostic_case("before_fragmented", True)
                await self.resource_gate()
                self.gate("diagnostic_resources_before", "passed")
                self.start_diagnostic_background()
                await asyncio.sleep(1)
                await self.fault("attachment")
                await self.drain()
                await asyncio.sleep(1)
                self.baseline = await self.sample()  # warmed, drained generation-2 baseline
                self.start_diagnostic_background()
                await self.diagnostic_case("after_unfragmented", False)
                await self.diagnostic_case("after_fragmented", True)
                await self.resource_gate()
                self.gate("diagnostic_resources_after", "passed")
                passed = all(case["passed"] for case in self.metadata["diagnostic_cases"])
                self.gate("diagnostic_cases", "passed" if passed else "failed")
                self.gate("required_metrics", "passed")
                self.gate("forwarding_coverage", "passed", combinations=list(SERVICES))
                self.gate("integrity_and_steady_state", "passed" if self.counts["unexpected_failures"] == 0 else "failed")
                self.transport_gates()
        finally:
            self.metadata["workload_seconds"] = time.monotonic() - self.work_started
            if self.load_interval_started is not None:
                self.load_intervals.append(time.monotonic() - self.load_interval_started)
                self.load_interval_started = None
            self.load_time = max(0, sum(self.load_intervals) - self.recovery_time)
            for task in self.tasks:
                task.cancel()
            await asyncio.gather(*self.tasks, return_exceptions=True)
            self.tasks = []

    async def set_mixed_background(self, duplex_count, *, reset=False):
        if reset:
            self.diagnostic_background = {}
            self.stop_workers = asyncio.Event()
            self.load_interval_started = time.monotonic()
        desired = {(name, index) for name in SERVICES for index in range(3)} - {
            (name, index) for name, index, _ in mixed_flows(duplex_count)}
        for key in set(self.diagnostic_background) - desired:
            task, stopped = self.diagnostic_background.pop(key)
            stopped.set()
            await asyncio.wait_for(task, 15)
        for name, index in sorted(desired - set(self.diagnostic_background)):
            stopped = asyncio.Event()
            probe = (name, index) == ("uu", 2)
            task = asyncio.create_task(self.worker(name, index, small_only=True, stopped=stopped,
                                                  fixed_size=1 if probe else 4096, probe=probe))
            self.diagnostic_background[name, index] = (task, stopped)
        self.tasks = [task for task, _ in self.diagnostic_background.values()]
        self.event("mixed_background", workers=sorted(desired), probe=["uu", 2], probe_persistent=True,
                   probe_payload_bytes=1, other_payload_bytes=4096, large_streams=4 + duplex_count, total_streams=12)

    async def mixed_diagnostic(self):
        self.work_started = time.monotonic()
        self.work_deadline = self.work_started + self.profile["duration"]
        self.metadata["diagnostic_scope"] = "4 ordinary plus 0/4/7 duplex 1 MiB streams; 12 total; persistent tiny probe; A-B-C-C-B-A; no restarts; controlled overlap, not original staggered soak"
        kind = getattr(self.args, "diagnostic_kind", "fragmentation")
        transport = kind == "transport"
        staggered = kind in ("staggered", "transport")
        if staggered:
            self.metadata["diagnostic_scope"] = "4 ordinary plus 7 duplex 1 MiB streams; 12 warmed connections including persistent tiny probe; sync-stagger-stagger-sync; one peer lifetime; observed soak offsets, not original periodic state"
        if transport:
            self.metadata["diagnostic_scope"] = "initial high burst, four sequential conditioning exchanges, conditioned high burst, 15s application-quiet, postquiet high burst; persistent checked probe continuity, official QUIC identity not exposed"
            if getattr(self.args, "no_conditioning_control", False):
                self.metadata["diagnostic_scope"] = "initial high burst, 5s wait without explicit large-transfer conditioning, unconditioned high burst, 15s application-quiet, postquiet high burst; historical elapsed-time control, not randomized; initial burst and tiny probe still supply prior traffic"
        try:
            async with asyncio.timeout_at(self.work_deadline):
                await self.resource_gate()
                self.gate("diagnostic_resources_before", "passed")
                await self.set_mixed_background(7 if staggered else 0, reset=True)
                for label, count in zip(self.profile["cases"], (7, 7, 7, 7) if staggered else (0, 4, 7, 7, 4, 0)):
                    if transport and label == "conditioned_high":
                        await self.transport_conditioning()
                    if transport and label == "unconditioned_high":
                        await self.transport_control_wait()
                    if transport and label == "post_quiet_high":
                        await self.transport_quiet()
                    await self.set_mixed_background(count)
                    options = {"offsets": staggered_offsets() if label.startswith("staggered") else [0.0] * 11} if staggered else {}
                    await self.diagnostic_case(label, mixed_duplex_count=count, **options)
                await self.resource_gate()
                self.gate("diagnostic_resources_after", "passed", scope="same host and attachment lifetimes; no rebaseline")
                cases = self.metadata["diagnostic_cases"]
                passed = all(case["passed"] for case in cases)
                probe_passed = all(case["probe"].get("success_during_large_window", 0) > 0 for case in cases)
                overlap_passed = all(case["observed_peak_large_operations"] == 4 + case["duplex_large_streams"]
                                     for case in cases if transport or not staggered or case["case"].startswith("sync"))
                if transport:
                    continuity = self.probe_connections == 1 and self.counts["diagnostic_probe_success"] > 0
                    self.gate("persistent_probe_continuity", "passed" if continuity else "not_exercised",
                              connections=self.probe_connections, note="checked application stream continuity; opaque QUIC connection ID unavailable")
                self.metadata["mixed_probe_stats"] = self.mixed_probe_stats
                self.gate("diagnostic_cases", "passed" if passed else "failed")
                self.gate("mixed_probe_observation", "passed" if probe_passed else "not_exercised")
                self.gate("mixed_overlap_coverage", "passed" if overlap_passed else "not_exercised")
                if staggered:
                    scheduling_passed = all(len(case["flows"]) == 11 and all(
                        frame.get("large_transfer_exercised") and 0 <= frame.get("start_lateness_seconds", -1) <= 0.25
                        for frame in case["flows"]) for case in cases)
                    self.gate("burst_schedule_coverage" if transport else "staggered_schedule_coverage", "passed" if scheduling_passed else "not_exercised",
                              max_allowed_start_lateness_seconds=0.25,
                              note="staggered rounds report actual overlap without requiring peak 11")
                    resources_passed = all(name in self.resource_coverage and all(
                        self.resource_coverage[name][field] > 0 for field in ("cpu_intervals", "cgroup_samples", "socket_samples"))
                        for name in ("attachment", "host", "fixtures"))
                    self.gate("resource_observation_coverage", "passed" if resources_passed else "not_exercised",
                              processes=self.resource_coverage,
                              limitations="shared cgroups, visible ancestors only; Unix queue bytes unavailable from proc")
                self.gate("required_metrics", "passed")
                self.gate("forwarding_coverage", "passed", combinations=list(SERVICES))
                self.gate("integrity_and_steady_state", "passed" if self.counts["unexpected_failures"] == 0 else "failed")
                self.transport_gates()
        finally:
            self.metadata["mixed_probe_stats"] = self.mixed_probe_stats
            self.metadata["workload_seconds"] = time.monotonic() - self.work_started
            if self.load_interval_started is not None:
                self.load_intervals.append(time.monotonic() - self.load_interval_started)
                self.load_interval_started = None
            self.load_time = max(0, sum(self.load_intervals) - self.recovery_time)
            for task in self.tasks:
                task.cancel()
            await asyncio.gather(*self.tasks, return_exceptions=True)
            self.tasks = []
            self.diagnostic_background = {}

    async def transport_conditioning(self):
        self.mixed_case = "conditioning"
        records = []
        sampler_stop = asyncio.Event()
        sampler = asyncio.create_task(self.diagnostic_sampling(sampler_stop))
        self.event("conditioning_start", transfers=4, simultaneous_large_limit=1)
        try:
            for index, name in enumerate(SERVICES):
                self.check_background()
                record = await self.diagnostic_transfer("conditioning", index, name, False, False)
                records.append(record)
            self.check_background()
            self.metadata["conditioning"] = records
            self.gate("conditioning", "passed" if all(record["outcome"] == "success" for record in records) else "failed")
            self.event("conditioning_end", passed=self.gates["conditioning"]["status"] == "passed")
        finally:
            self.mixed_case = None
            sampler_stop.set()
            await asyncio.wait_for(sampler, 15)

    async def transport_control_wait(self):
        require(not self.active_transfers, "control wait must not overlap outstanding foreground transfers")
        began = time.monotonic()
        self.mixed_case = "no_conditioning_control"
        sampler_stop = asyncio.Event()
        sampler = asyncio.create_task(self.diagnostic_sampling(sampler_stop))
        self.event("no_conditioning_control_start", planned_seconds=5, explicit_conditioning_large_transfers=0,
                   note="tiny probe and numeric observations continue; initial high burst already supplied traffic")
        try:
            await asyncio.sleep(5)
            self.check_background()
        finally:
            sampler_stop.set()
            await asyncio.wait_for(sampler, 15)
            self.mixed_case = None
        elapsed = time.monotonic() - began
        self.metadata["conditioning"] = []
        self.gate("no_conditioning_control", "passed", planned_seconds=5, observed_seconds=elapsed,
                  explicit_conditioning_large_transfers=0,
                  historical_conditioning_seconds=[5.023618920007721, 4.614388588117436],
                  note="approximate historical time match; sampler completion adds latency; not evidence of an unprimed peer")
        self.event("no_conditioning_control_end", seconds=elapsed)

    async def transport_quiet(self):
        # Acknowledge only between completed probe exchanges: no outstanding
        # application bytes may be attributed to the quiet interval.
        self.probe_resume.clear()
        self.probe_pause_requested.set()
        try:
            await asyncio.wait_for(self.probe_paused.wait(), 12)
            before = self.counts["diagnostic_probe_success"]
            began = time.monotonic()
            if self.load_interval_started is not None:
                self.load_intervals.append(began - self.load_interval_started)
                self.load_interval_started = None
            self.event("application_quiet_start", duration_seconds=15, probe_connections=self.probe_connections)
            while time.monotonic() - began < 15:
                self.check_background()
                await self.sample()
                await asyncio.sleep(min(1, max(0, 15 - (time.monotonic() - began))))
            require(self.counts["diagnostic_probe_success"] == before, "probe generated bytes during application-quiet gate")
            elapsed = time.monotonic() - began
            self.gate("application_quiet", "passed", seconds=elapsed,
                      note="no workload application traffic; transport keepalives/control/metrics may continue")
            self.event("application_quiet_end", seconds=elapsed)
        finally:
            self.probe_pause_requested.clear()
            self.probe_resume.set()
            self.load_interval_started = time.monotonic()

    async def set_duplex_background(self, count, *, reset=False):
        if reset:
            self.diagnostic_background = {}
            self.stop_workers = asyncio.Event()
            self.load_interval_started = time.monotonic()
        # Foreground routes are UT, UU, and (in the three-flow case) TU.
        desired = {(name, index) for name in SERVICES for index in range(3)} - {("ut", 1), ("uu", 1)}
        if count == 3:
            desired.remove(("tu", 1))
        for key in set(self.diagnostic_background) - desired:
            task, stopped = self.diagnostic_background.pop(key)
            stopped.set()
            await asyncio.wait_for(task, 15)  # retire only the extra small worker, not all connections
        for name, index in desired - set(self.diagnostic_background):
            stopped = asyncio.Event()
            task = asyncio.create_task(self.worker(name, index, small_only=True, stopped=stopped,
                                                   fixed_size=1 if (name, index) == ("tt", 0) else None))
            self.diagnostic_background[name, index] = (task, stopped)
        self.tasks = [task for task, _ in self.diagnostic_background.values()]
        self.event("duplex_background", workers=sorted(desired), probe=["tt", 0],
                   probe_payload_bytes=1, other_payload_bytes=4096, foreground_large_streams=count,
                   total_streams=12, post_recovery_drain=False)

    async def duplex_diagnostic(self):
        self.work_started = time.monotonic()
        self.work_deadline = self.work_started + self.profile["duration"]
        self.metadata["diagnostic_scope"] = "2 versus 3 synchronized duplex 1 MiB streams; 12 total; tiny shared-peer probe; no post-recovery drain"
        original_host_baseline = self.baseline["host"]
        try:
            async with asyncio.timeout_at(self.work_deadline):
                await self.set_duplex_background(2, reset=True)
                await self.diagnostic_case("before_two_duplex", duplex_count=2)
                await self.set_duplex_background(3)
                await self.diagnostic_case("before_three_duplex", duplex_count=3)
                await self.resource_gate()
                self.gate("diagnostic_resources_before", "passed")
                await self.set_duplex_background(3, reset=True)
                await asyncio.sleep(1)
                await self.fault("attachment")
                # Preserve pending/recovered small-worker connections. Do not
                # drain or rebaseline the new attachment before the comparison.
                await self.set_duplex_background(2)
                await self.diagnostic_case("after_two_duplex", duplex_count=2)
                await self.set_duplex_background(3)
                await self.diagnostic_case("after_three_duplex", duplex_count=3)
                await self.drain()
                await asyncio.sleep(2)
                final = await self.sample()
                host_growth = same_lifetime_growth(original_host_baseline, final["host"], self.args.max_fd_growth)
                self.gate("diagnostic_resources_after", "passed", host_fd_growth=host_growth,
                          attachment_note="new-generation counts diagnostic only; attachment leak gate ran before restart")
                self.metadata["post_restart_drained_resources"] = final
                passed = all(case["passed"] for case in self.metadata["diagnostic_cases"])
                self.gate("diagnostic_cases", "passed" if passed else "failed")
                self.gate("required_metrics", "passed")
                self.gate("forwarding_coverage", "passed", combinations=list(SERVICES))
                self.gate("integrity_and_steady_state", "passed" if self.counts["unexpected_failures"] == 0 else "failed")
                self.transport_gates()
        finally:
            self.metadata["workload_seconds"] = time.monotonic() - self.work_started
            if self.load_interval_started is not None:
                self.load_intervals.append(time.monotonic() - self.load_interval_started)
                self.load_interval_started = None
            self.load_time = max(0, sum(self.load_intervals) - self.recovery_time)
            for task in self.tasks:
                task.cancel()
            await asyncio.gather(*self.tasks, return_exceptions=True)
            self.tasks = []

    async def workload(self):
        if self.args.mode == "smoke":
            return await self.smoke()
        if self.args.mode == "diagnostic":
            return await self.diagnostic()
        self.work_started = time.monotonic()
        self.work_deadline = self.work_started + self.profile["duration"]
        quiet = asyncio.create_task(self.quiet()) if self.args.mode == "soak" else None
        faults = collections.deque(self.profile["faults"])
        checkpoint = False
        current_level = 0
        total = self.profile["levels"][0] if self.args.mode == "stress" else self.profile["concurrency"]
        self.start_workers(total)
        next_sample = 0
        try:
            # Deadline bounds all recovery/drain work, not just successful traffic.
            async with asyncio.timeout_at(self.work_deadline):
                while True:
                    elapsed = time.monotonic() - self.work_started
                    if elapsed >= self.profile["duration"] - 15:
                        break
                    for task in self.tasks:
                        if task.done():
                            task.result()
                            raise AssertionError("traffic worker exited early")
                    if quiet and quiet.done():
                        quiet.result()
                    if self.heartbeat_task.done():
                        self.heartbeat_task.result()
                    if not checkpoint and elapsed >= self.profile["checkpoint"]:
                        if quiet:
                            require(quiet.done(), "quiet gate must complete before resource drain")
                            quiet.result()
                        await self.resource_gate()
                        checkpoint = True
                        if self.args.mode == "soak":
                            self.start_workers(total)
                    if self.args.mode == "stress" and not checkpoint:
                        level = min(int(elapsed / (self.profile["duration"] / len(self.profile["levels"]))),
                                    len(self.profile["levels"]) - 1)
                        if level != current_level:
                            await self.drain()
                            current_level = level
                            total = self.profile["levels"][level]
                            self.start_workers(total)
                    if faults and elapsed >= faults[0][0]:
                        _, scope = faults.popleft()
                        await self.fault(scope)
                    if elapsed >= next_sample:
                        await self.sample()
                        next_sample = time.monotonic() - self.work_started + (1 if self.args.profile == "dev" else 5)
                    await asyncio.sleep(0.1)
                require(checkpoint and not faults, "required scheduled phases did not finish")
                if quiet:
                    require(quiet.done(), "quiet gate did not finish")
                    quiet.result()
                await self.drain()
                await asyncio.sleep(0.3)  # fixture counter checkpoint flush
                await self.sample()
                require(self.counts["unexpected_failures"] == 0 and self.counts["drain_failures"] == 0,
                        "unexpected I/O or resource-drain failures")
                # Consume the requested phase budget without inventing successful
                # traffic during drain. Setup/cleanup remain separate.
                await asyncio.sleep(max(0, self.work_deadline - time.monotonic() - 0.05))
        finally:
            self.metadata["workload_seconds"] = time.monotonic() - self.work_started
            if self.load_interval_started is not None:
                self.load_intervals.append(time.monotonic() - self.load_interval_started)
                self.load_interval_started = None
            self.load_time = max(0, sum(self.load_intervals) - self.recovery_time)
            self.stop_workers.set()
            for task in self.tasks:
                task.cancel()
            if quiet and not quiet.done():
                quiet.cancel()
            await asyncio.gather(*self.tasks, *([quiet] if quiet else []), return_exceptions=True)
            self.tasks = []
        require(self.counts["unexpected_failures"] == 0 and self.counts["drain_failures"] == 0,
                "unexpected I/O or resource-drain failures")
        require(all(self.counts[f"{name}.success"] > 0 for name in SERVICES), "missing forwarding coverage")
        require(self.counts["success"] > 0, "no successful traffic")
        if getattr(self.args, "workload", "capacity") == "supported-admin":
            self.admin_bulk_gate()
        self.gate("integrity_and_steady_state", "passed")
        self.gate("forwarding_coverage", "passed", combinations=list(SERVICES))
        self.gate("required_metrics", "passed")
        self.transport_gates()

    async def collect(self):
        manifest = await self.control.rpc("manifest", timeout=5)
        require(len(manifest) <= 20, "target artifact manifest exceeds bound")
        parent = self.artifacts.root / "target"
        parent.mkdir(mode=0o700, exist_ok=True)
        for name, info in manifest.items():
            destination = (parent / name).resolve()
            require(destination.is_relative_to(parent) and 0 <= info["size"] <= 8 * 1024 * 1024,
                    "invalid target artifact manifest")
            destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            digest = hashlib.sha256()
            offset = 0
            with open(destination, "xb", opener=lambda p, f: os.open(p, f, 0o600)) as output:
                while offset < info["size"]:
                    chunk = await self.control.rpc("artifact", dict(name=name, offset=offset), timeout=5)
                    data = base64.b64decode(chunk["data"], validate=True)
                    require(data and len(data) <= 128 * 1024 and offset + len(data) <= info["size"],
                            "partial or oversized target artifact transfer")
                    output.write(data)
                    digest.update(data)
                    offset += len(data)
            require(digest.hexdigest() == info["sha256"], "target artifact checksum mismatch")
        require("cleanup.json" in manifest and "metadata.json" in manifest, "required target artifacts missing")
        self.collected = True
        self.gate("artifact_transfer", "passed")

    async def cleanup(self):
        errors = []
        if self.heartbeat_task:
            self.heartbeat_task.cancel()
            await asyncio.gather(self.heartbeat_task, return_exceptions=True)
        try:
            async with asyncio.timeout(CLEANUP):
                if self.attachment:
                    try:
                        await self.attachment.stop()
                        self.cleanup_status["controller"] = "verified"
                    except Exception as error:
                        errors.append(f"controller stop: {type(error).__name__}")
                else:
                    self.cleanup_status["controller"] = "verified"
                if self.control.child and self.control.root:
                    try:
                        if self.control.broken or self.control.child.returncode is not None:
                            await self.control.reconnect()
                        if self.failed_frames:
                            # Capture only test frame IDs/high-water values, never
                            # relay credentials or payloads. This is a later view,
                            # not target receipt timing at the client deadline.
                            try:
                                remote = await self.control.rpc("sample", timeout=10)
                                frames = []
                                for frame in self.failed_frames:
                                    backend = "unix" if frame["combination"].endswith("u") else frame["combination"]
                                    frames.append(dict(identity=frame["identity"], combination=frame["combination"],
                                                       requested_sequence=frame["sequence"],
                                                       last_validated_sequence=remote["fixtures"][backend]["highwater"].get(frame["identity"])))
                                self.metadata["failure_target_snapshot"] = dict(captured_controller_utc=time.time(), frames=frames,
                                    scope="after controller reaping, before target stop; eventual validation, not deadline timing")
                            except Exception as snapshot_error:
                                self.metadata["failure_target_snapshot"] = dict(error_type=type(snapshot_error).__name__)
                                if self.control.broken:
                                    await self.control.reconnect()
                        receipt = await self.control.rpc("stop", timeout=20)
                        self.cleanup_status["target"] = receipt["cleanup"]
                        errors.extend(receipt["errors"])
                        await self.collect()
                        await self.control.rpc("finish", timeout=5)
                        await asyncio.wait_for(self.control.child.wait(), 5)
                        require(self.control.child.returncode == 0, "target runtime removal not confirmed")
                        self.runtime_removed = True
                    except Exception as error:
                        errors.append(f"target cleanup/collection: {type(error).__name__}")
                else:
                    errors.append("target cleanup unverified")
        except asyncio.TimeoutError:
            errors.append("cleanup deadline exceeded")
        finally:
            try:
                async with asyncio.timeout(FORCE_REAP):
                    if self.attachment:
                        await self.attachment.force_reap()
                        self.cleanup_status["controller"] = "verified"
                    await self.control.close()
            except Exception as error:
                errors.append(f"forced reap: {type(error).__name__}")
            shutil.rmtree(self.root)
        if not self.collected:
            self.gate("artifact_transfer", "failed", reason="complete checksum-verified transfer unavailable")
        self.gate("owned_process_cleanup", "passed" if all(v == "verified" for v in self.cleanup_status.values())
                  and not errors and self.runtime_removed else "failed", machines=self.cleanup_status,
                  target_runtime_removed=self.runtime_removed, errors=errors)
        if self.attachment:
            self.metadata["controller_exit"] = dict(pid=self.attachment.pid,
                        guard_pid=self.attachment.child.pid if self.attachment.child else None,
                        generation=self.attachment.generation, start_identity=self.attachment.start_identity,
                        exit_status=self.attachment.child.returncode if self.attachment.child else None,
                        log_categories=dict(self.attachment.categories))
        return errors

    def report(self, error, cleanup_errors):
        required = ["readiness", "integrity_and_steady_state", "forwarding_coverage", "required_metrics",
                    "transport_observation",
                    "same_lifetime_fd_attachment", "same_lifetime_fd_host", "artifact_transfer", "owned_process_cleanup"]
        if self.relay_policy:
            required.append("custom_relay_observation")
        if getattr(self.args, "workload", "capacity") == "supported-admin" and self.args.mode in ("stress", "soak"):
            required.append("supported_bulk_coverage")
        if getattr(self.args, "workload", "capacity") == "supported-admin":
            required.append("warmed_fd_baseline")
        if self.args.mode == "soak":
            required += (["real_365s_quiet", "default_300s_expiry"] if self.args.profile == "acceptance"
                         else ["dev_quiet", "dev_short_expiry"])
            required += [f"recovery_{scope}" for _, scope in self.profile["faults"]]
        if self.args.mode == "diagnostic":
            required += ["diagnostic_cases", "diagnostic_resources_before", "diagnostic_resources_after"]
            if getattr(self.args, "diagnostic_kind", "fragmentation") in ("mixed", "staggered", "transport"):
                required += ["mixed_probe_observation", "mixed_overlap_coverage"]
                if self.args.diagnostic_kind == "staggered":
                    required += ["staggered_schedule_coverage", "resource_observation_coverage"]
                if self.args.diagnostic_kind == "transport":
                    required += ["burst_schedule_coverage", "resource_observation_coverage",
                                 "no_conditioning_control" if getattr(self.args, "no_conditioning_control", False) else "conditioning",
                                 "persistent_probe_continuity", "application_quiet"]
            else:
                required.append("recovery_attachment")
        for name in required:
            if name not in self.gates:
                self.gate(name, "not_exercised", reason="run ended before gate completed")
        passed = not error and not cleanup_errors and all(self.gates[g]["status"] == "passed" for g in required)
        self.metadata.update(phase_timing=dict(requested_workload=self.profile["duration"],
                                             achieved_load_time=self.load_time, recovery_pauses=self.recovery_time,
                                             workload_elapsed=self.metadata.get("workload_seconds", 0),
                                             total_wall_time=time.monotonic() - self.started),
                             gates=self.gates, artifact_bounds=dict(log_bytes_per_process=1024 * 1024,
                                                                    detailed_operations=2000, latency_samples=10000,
                                                                    metric_records_per_machine=20000),
                             provenance_note="Provenance/commit labels are operator declarations; hashes and versions are measured.",
                              power_state="unverified: no global power changes or sleep assertions")
        self.metadata["workload"] = getattr(self.args, "workload", "capacity")
        self.metadata["supported_bulk_operations"] = self.admin_bulk_results
        self.artifacts.json("metadata.json", self.metadata)
        latency = sorted(self.latencies)
        self.artifacts.json("summary.json", dict(run_id=self.run_id, result="passed" if passed else "failed",
                            workload=getattr(self.args, "workload", "capacity"),
                            suite_role="supported_workload_acceptance" if getattr(self.args, "workload", "capacity") == "supported-admin" else "capacity_characterization",
                            product_release_certification=False,
                            acceptance_certified=passed and self.args.profile == "acceptance",
                            error=error, cleanup_errors=cleanup_errors, gates=self.gates, counts=dict(self.counts),
                            latency_seconds={str(q): latency[min(len(latency) - 1, int(len(latency) * q))]
                                             for q in (0.5, 0.95, 0.99)} if latency else {},
                            truncated_details=dict(self.artifacts.truncated), timing=self.metadata["phase_timing"]))
        return passed


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("mode", choices=("smoke", "stress", "soak", "diagnostic"))
    result.add_argument("--profile", choices=("dev", "acceptance"), default="dev")
    result.add_argument("--workload", choices=("capacity", "supported-admin"), default="capacity",
                        help="explicit workload contract; existing capacity behavior remains the default")
    result.add_argument("--duration", type=duration)
    result.add_argument("--levels", type=levels, help="TOTAL concurrency, distributed across all four services")
    result.add_argument("--concurrency", type=int, default=12, help="total soak traffic workers")
    result.add_argument("--pace", type=float, default=0.1, help="minimum pause per worker operation (seconds)")
    result.add_argument("--controller-binary")
    result.add_argument("--target-binary", help="existing target path copied privately; never overwritten")
    result.add_argument("--binary-profile", choices=("baseline", "candidate", "release"), default="baseline")
    result.add_argument("--control-socket", help="private administrative bridge socket, carried over verified SSH")
    result.add_argument("--run-id", help="explicit run UUID for administrative bridging")
    result.add_argument("--relay-url", help="run-scoped custom HTTPS relay base URL")
    result.add_argument("--relay-token-env", default="LOCHO_RELAY_TOKEN")
    result.add_argument("--include-n0-relays", action="store_true", help="explicitly add N0 fallback candidates")
    result.add_argument("--disable-relay-qad", action="store_true", help="explicit HTTPS-relay profile without QAD")
    result.add_argument("--transfer-telemetry", action="store_true", help="bounded, non-destructive I/O phase observations")
    result.add_argument("--fixture-observation", action="store_true",
                        help="bounded target large-frame progress; also enables client transfer telemetry")
    result.add_argument("--diagnostic-kind", choices=("fragmentation", "duplex", "mixed", "staggered", "transport"), default="fragmentation")
    result.add_argument("--no-conditioning-control", action="store_true", help="transport diagnostic only: replace sequential conditioning with a 5s observed wait")
    for role in ("controller", "target"):
        result.add_argument(f"--{role}-provenance", choices=("official", "clean", "modified", "unknown"), default="unknown")
        result.add_argument(f"--{role}-source-commit", default="unknown")
        result.add_argument(f"--{role}-relay-env", help="explicitly approved environment file on that machine only")
    result.add_argument("--ssh-host", default="127.0.0.1")
    result.add_argument("--ssh-port", type=int, default=2223)
    result.add_argument("--ssh-user")
    result.add_argument("--known-hosts")
    result.add_argument("--target-python", default="python3")
    result.add_argument("--target-runtime-parent", help="existing short private temporary parent on target")
    result.add_argument("--output", default="artifacts")
    result.add_argument("--fd-soft", type=int, default=1024)
    result.add_argument("--fd-hard", type=int, default=4096)
    result.add_argument("--max-fd-growth", type=int, default=16)
    result.add_argument("--dry-run", action="store_true", help="offline JSON preview; no SSH, processes or files")
    result.add_argument("--approve-execution", action="store_true", help="explicitly authorize the reviewed remote workload/fault scope")
    return result


async def main(args, profile, run_id):
    run = Run(args, profile, run_id)
    error = None
    interrupted = asyncio.Event()
    current = asyncio.current_task()
    def interrupt():
        interrupted.set()
        current.cancel()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, interrupt)
    try:
        async with asyncio.timeout_at(run.started + profile["outer_budget"] - CLEANUP - FORCE_REAP):
            async with asyncio.timeout(SETUP):
                await run.setup()
            run.metadata["setup_seconds"] = time.monotonic() - run.started
            await run.workload()
    except BaseException as exception:
        error = "interrupted" if interrupted.is_set() else run.artifacts.redact(
            f"{type(exception).__name__}: {exception}", runtime=True)[:2048]
        run.event("run_failed", error_type=error)
    finally:
        # Further interrupts cannot skip bounded owned-child finalization.
        for sig in (signal.SIGINT, signal.SIGTERM):
            loop.remove_signal_handler(sig)
            signal.signal(sig, signal.SIG_IGN)
        cleanup_errors = await run.cleanup()
    passed = run.report(error, cleanup_errors)
    print(json.dumps(dict(result="passed" if passed else "failed", artifacts=str(run.artifacts.root),
                          acceptance_certified=passed and args.profile == "acceptance")))
    return 0 if passed else 1


if __name__ == "__main__":
    cli = parser()
    arguments = cli.parse_args()
    try:
        require(0.01 <= arguments.pace <= 10, "pacing must be 0.01–10 seconds")
        require(64 <= arguments.fd_soft <= arguments.fd_hard <= 65536, "invalid descriptor limits")
        require(0 <= arguments.max_fd_growth <= 1024, "invalid descriptor drain allowance")
        require(1 <= arguments.ssh_port <= 65535, "invalid SSH port")
        profile = schedule(arguments)
        policy = relay_policy(arguments)
        if policy and not arguments.dry_run:
            require(arguments.controller_relay_env and arguments.target_relay_env,
                    "custom relay execution requires approved environment-file paths on both machines")
        run_id = arguments.run_id or str(uuid.uuid4())
        require(str(uuid.UUID(run_id)) == run_id, "invalid run UUID")
        if arguments.dry_run:
            print(json.dumps(dry_run(arguments, profile, run_id), indent=2))
            sys.exit(0)
        require(arguments.approve_execution, "review --dry-run, then explicitly pass --approve-execution")
        require(arguments.controller_binary and arguments.target_binary and arguments.ssh_user,
                "execution requires both binary paths and SSH user")
        if arguments.binary_profile == "baseline":
            require(arguments.controller_provenance == "modified" and arguments.target_provenance == "official",
                    "baseline requires --controller-provenance modified --target-provenance official")
        if arguments.profile == "acceptance":
            require(all(getattr(arguments, f"{role}_source_commit") != "unknown"
                        and getattr(arguments, f"{role}_provenance") != "unknown"
                        for role in ("controller", "target")),
                    "acceptance requires both declared source commits and provenances")
    except AssertionError as error:
        cli.error(str(error))
    sys.exit(asyncio.run(main(arguments, profile, run_id)))
