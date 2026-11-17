#!/usr/bin/env python3
"""Administrative launcher: both measured machines are remote, not this machine.

Control RPC crosses two authenticated SSH legs using private Unix forwarding.
No agent forwarding, copied SSH keys, installed executables or production config.
"""

import argparse
import asyncio
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import tarfile
import tempfile
import time
import urllib.request
import uuid

from native_support import Artifacts, MESSAGE_LIMIT, encode, private_json, receive, relay_policy, validate_message
from two_machine import SSHControl, parser as workload_parser, schedule
from unix_streams import require


TARGET_PLATFORMS = {
    "aarch64-apple-darwin": ("Darwin", "arm64"),
    "x86_64-unknown-linux-gnu": ("Linux", "x86_64"),
}


STAGE_CONTROLLER = r'''
import base64,hashlib,json,os,pathlib,shutil,sys,tempfile
value=json.loads(sys.stdin.buffer.readline(2097153))
root=pathlib.Path(tempfile.mkdtemp(prefix="lc-",dir=value["parent"])).resolve(); root.chmod(0o700)
try:
    for name,data in value["files"].items():
        if name not in ("two_machine.py","remote_runner.py","native_support.py","unix_streams.py"): raise RuntimeError("invalid stage file")
        fd=os.open(root/name,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
        with os.fdopen(fd,"wb") as out: out.write(base64.b64decode(data,validate=True))
    size=value["binary_size"]
    if not 0<size<=67108864: raise RuntimeError("binary staging exceeds bound")
    fd=os.open(root/"locho",os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o700); digest=hashlib.sha256()
    with os.fdopen(fd,"wb") as out:
        while size:
            chunk=sys.stdin.buffer.read(min(size,131072))
            if not chunk: raise RuntimeError("partial binary stage")
            out.write(chunk); digest.update(chunk); size-=len(chunk)
    if digest.hexdigest()!=value["binary_sha256"]: raise RuntimeError("binary staging checksum mismatch")
    fd=os.open(root/"owner.json",os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
    with os.fdopen(fd,"w") as out: json.dump({"run_id":value["run_id"]},out)
    print(json.dumps({"root":str(root),"run_id":value["run_id"]}),flush=True)
except BaseException:
    shutil.rmtree(root)
    raise
'''

COLLECT_CONTROLLER = r'''
import hashlib,json,pathlib,sys
root=pathlib.Path(sys.argv[1]).resolve(); run_id=sys.argv[2]
if json.loads((root/"owner.json").read_text())["run_id"]!=run_id: raise RuntimeError("ownership mismatch")
dirs=list((root/"artifacts").glob("two-machine-*-"+run_id))
if len(dirs)!=1: raise RuntimeError("missing controller artifact directory")
source=dirs[0]; files=[]
for path in sorted(source.rglob("*")):
    if path.is_symlink(): raise RuntimeError("artifact symlink refused")
    if path.is_file():
        if path.stat().st_size>8388608: raise RuntimeError("artifact file exceeds bound")
        files.append({"name":str(path.relative_to(source)),"size":path.stat().st_size,"sha256":hashlib.sha256(path.read_bytes()).hexdigest()})
if len(files)>64 or sum(f["size"] for f in files)>67108864: raise RuntimeError("artifact volume exceeds bound")
print(json.dumps({"files":files,"source":str(source)}),flush=True)
for item in files:
    with open(source/item["name"],"rb") as stream:
        while chunk:=stream.read(131072): sys.stdout.buffer.write(chunk)
sys.stdout.buffer.flush()
'''

REMOVE_CONTROLLER = r'''
import json,pathlib,shutil,sys
root=pathlib.Path(sys.argv[1]).resolve(); run_id=sys.argv[2]
if json.loads((root/"owner.json").read_text())["run_id"]!=run_id: raise RuntimeError("ownership mismatch")
shutil.rmtree(root)
print(json.dumps({"removed":True,"run_id":run_id}))
'''

RELAY_SAMPLE = r'''
import json,pathlib,subprocess,sys,time,tomllib,urllib.request
selected={"relayserver_bytes_recv_total","relayserver_bytes_sent_total","relayserver_bytes_rx_ratelimited_total","relayserver_conns_rx_ratelimited_total","relayserver_send_packets_dropped_total","relayserver_unique_client_keys_total"}
for command in sys.stdin:
    if command.strip()!="sample": break
    cfg=tomllib.loads(pathlib.Path("/etc/iroh-relay/config.toml").read_text())
    status=subprocess.run(["systemctl","show","iroh-relay.service","--property=MainPID,ActiveState,SubState"],capture_output=True,text=True,timeout=3)
    service=dict(line.split("=",1) for line in status.stdout.splitlines() if "=" in line)
    pid=service.get("MainPID","0")
    values=subprocess.run(["ps","-p",pid,"-o","rss=,pcpu=,etimes="],capture_output=True,text=True,timeout=3).stdout.split()
    if service.get("ActiveState")!="active" or len(values)!=3: raise RuntimeError("relay process metrics unavailable")
    limits=cfg.get("limits",{}).get("client",{}).get("rx",{})
    numeric={key:limits.get(key) if isinstance(limits.get(key),(int,float)) else None for key in ("bytes_per_second","max_burst_bytes")}
    with urllib.request.urlopen("http://127.0.0.1:9090/metrics",timeout=3) as response: raw=response.read(2097153)
    if len(raw)>2097152: raise RuntimeError("relay metrics exceed bound")
    counters={}
    for line in raw.decode().splitlines():
        if not line or line.startswith("#"): continue
        name=line.split("{",1)[0].split(" ",1)[0]
        if name in selected: counters[name]=counters.get(name,0)+float(line.split()[-1])
    print(json.dumps({"utc":time.time(),"sample_monotonic":time.monotonic(),"process":"relay","pid":int(pid),"rss_kib":int(values[0]),"cpu_percent":float(values[1]),"uptime_seconds":int(values[2]),"configured_client_rx_limits":numeric,"receive_limit_explicit":bool(limits),"shared_token_access_configured":isinstance(cfg.get("access"),dict) and "shared_token" in cfg["access"],"qad_configured":cfg.get("enable_quic_addr_discovery"),"counters":counters,"scope":"whole relay, including other clients"}),flush=True)
'''


def ssh_argv(destination, command, forwarding=None):
    require(destination and not destination.startswith("-") and "\n" not in destination, "invalid controller destination")
    args = ["ssh", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
            "-o", "ConnectTimeout=10", "-o", "ConnectionAttempts=1", "-o", "ServerAliveInterval=5",
            "-o", "ServerAliveCountMax=2", "-o", "ExitOnForwardFailure=yes"]
    if forwarding:
        args += ["-R", forwarding]
    return args + [destination, command]


async def spawn(argv):
    return await asyncio.create_subprocess_exec(*argv, stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE, limit=MESSAGE_LIMIT)


async def reap(child):
    if child and child.returncode is None:
        child.terminate()
        try:
            await asyncio.wait_for(child.wait(), 2)
        except asyncio.TimeoutError:
            child.kill()
            await asyncio.wait_for(child.wait(), 3)


class RelayMonitor:
    def __init__(self, args, artifacts):
        self.args, self.artifacts = args, artifacts
        self.child = self.task = None
        self.samples = 0
        self.error = None
        self.previous = None

    async def sample(self):
        async with asyncio.timeout(12):
            self.child.stdin.write(b"sample\n")
            await self.child.stdin.drain()
            record = await receive(self.child.stdout)
            require(record.get("process") == "relay" and isinstance(record.get("counters"), dict)
                    and {"relayserver_bytes_recv_total", "relayserver_bytes_sent_total",
                          "relayserver_bytes_rx_ratelimited_total", "relayserver_conns_rx_ratelimited_total",
                          "relayserver_send_packets_dropped_total"} <= set(record["counters"]),
                    "invalid relay metrics response")
            if self.previous:
                previous = self.previous
                elapsed = record["sample_monotonic"] - previous["sample_monotonic"]
                require(elapsed > 0 and record["pid"] == previous["pid"]
                        and record["uptime_seconds"] >= previous["uptime_seconds"], "relay restarted or invalid sample interval")
                delta = {key: value - previous["counters"][key] for key, value in record["counters"].items()
                         if key in previous["counters"]}
                require(all(value >= 0 for value in delta.values()), "relay metric counter reset")
                record.update(interval_seconds=elapsed, counter_deltas=delta,
                              bytes_per_second={key: delta[key] / elapsed for key in (
                                  "relayserver_bytes_recv_total", "relayserver_bytes_sent_total")})
            self.previous = record
            self.artifacts.event("metrics-relay.jsonl", record)
            self.samples += 1

    async def start(self):
        argv = ssh_argv(self.args.relay_monitor_ssh, shlex.join(["sudo", "-n", "python3", "-u", "-c", RELAY_SAMPLE]))
        extra = []
        if self.args.relay_monitor_key:
            extra += ["-i", self.args.relay_monitor_key]
        if self.args.relay_monitor_host_key_alias:
            extra += ["-o", f"HostKeyAlias={self.args.relay_monitor_host_key_alias}"]
        if self.args.relay_monitor_known_hosts:
            extra += ["-o", f"UserKnownHostsFile={self.args.relay_monitor_known_hosts}"]
        extra += ["-o", "CheckHostIP=no"]
        self.child = await spawn(argv[:1] + extra + argv[1:])
        await self.sample()  # required before staging load
        self.task = asyncio.create_task(self.loop())

    async def loop(self):
        try:
            while True:
                await asyncio.sleep(getattr(self.args, "relay_monitor_interval", 5))
                await self.sample()
        except asyncio.CancelledError:
            raise
        except Exception as error:
            self.error = type(error).__name__

    async def close(self):
        if self.task:
            self.task.cancel()
            await asyncio.gather(self.task, return_exceptions=True)
        if self.child and self.child.returncode is None and not self.error:
            try:
                await self.sample()  # terminal counter delta before closing the approved read-only sampler
            except Exception as error:
                self.error = type(error).__name__
        if self.child and self.child.stdin:
            self.child.stdin.close()
        await reap(self.child)


def release_binaries(runtime, target_platform="aarch64-apple-darwin", github_api_via_gh=False):
    """Fetch signed-infrastructure HTTPS release metadata, verify asset digests.

    No installer is run. Tar members are read, not extracted into arbitrary paths.
    """
    base = "https://api.github.com/repos/trchopan/locho/releases/tags/v1.3.0"
    if github_api_via_gh:
        response = subprocess.run(["gh", "api", "repos/trchopan/locho/releases/tags/v1.3.0"],
                                  capture_output=True, timeout=20)
        require(response.returncode == 0 and len(response.stdout) <= MESSAGE_LIMIT,
                "GitHub CLI release metadata fetch failed")
        release = json.loads(response.stdout)
    else:
        with urllib.request.urlopen(base, timeout=20) as response:
            release = json.load(response)
    require(not release["prerelease"] and release["tag_name"] == "v1.3.0", "stable release metadata mismatch")
    assets = {asset["name"]: asset for asset in release["assets"]}
    binaries = {}
    require(target_platform in TARGET_PLATFORMS, "unsupported target release platform")
    verified = {}
    for role, triple in (("controller", "x86_64-unknown-linux-gnu"), ("target", target_platform)):
        asset = assets[f"locho-{triple}.tar.xz"]
        require(asset["digest"].startswith("sha256:"), "release asset digest unavailable")
        if triple not in verified:
            with urllib.request.urlopen(asset["browser_download_url"], timeout=30) as response:
                archive = response.read(16 * 1024 * 1024 + 1)
            require(len(archive) <= 16 * 1024 * 1024, "release archive exceeds bound")
            require(hashlib.sha256(archive).hexdigest() == asset["digest"].split(":")[1], "release archive checksum mismatch")
            with tarfile.open(fileobj=io.BytesIO(archive), mode="r:xz") as source:
                members = [member for member in source.getmembers() if member.isfile() and Path(member.name).name == "locho"]
                require(len(members) == 1 and members[0].size <= 64 * 1024 * 1024, "release binary member invalid")
                verified[triple] = source.extractfile(members[0]).read()
        binary = verified[triple]
        path = runtime / f"locho-{role}"
        path.write_bytes(binary)
        path.chmod(0o700)
        binaries[role] = dict(path=path, sha256=hashlib.sha256(binary).hexdigest(), archive_sha256=asset["digest"],
                               source_commit=release["target_commitish"], provenance="official", platform=triple)
    return binaries


def candidate_binaries(runtime, binary, manifest_path):
    """Explicit private candidate input, never a release-signature claim."""
    manifest = json.loads(Path(manifest_path).read_text())
    require(manifest.get("base_commit") == "8342a7141bf27fb5b30adca224d759fa2a7aa0bc"
            and manifest.get("target") == "x86_64-unknown-linux-gnu"
            and manifest.get("feature") == "perf-quic-stats", "candidate manifest baseline/target/feature mismatch")
    for key in ("binary_sha256", "source_archive_sha256", "instrumentation_sha256"):
        value = manifest.get(key, "")
        require(len(value) == 64 and all(char in "0123456789abcdef" for char in value), "candidate manifest requires SHA-256 provenance")
    for name, key in (("candidate-source.tar.gz", "source_archive_sha256"), ("instrumentation.patch", "instrumentation_sha256")):
        path = Path(manifest_path).resolve().parent / name
        require(path.is_file() and path.stat().st_size <= 16 * 1024 * 1024
                and hashlib.sha256(path.read_bytes()).hexdigest() == manifest[key],
                "candidate retained source/instrumentation checksum mismatch")
    source = Path(binary).resolve()
    require(source.is_file() and source.stat().st_size <= 64 * 1024 * 1024, "invalid private candidate binary")
    data = source.read_bytes()
    require(hashlib.sha256(data).hexdigest() == manifest["binary_sha256"], "candidate binary checksum mismatch")
    result = {}
    for role in ("controller", "target"):
        path = runtime / ("candidate-" + role)
        path.write_bytes(data)
        path.chmod(0o700)
        result[role] = dict(path=path, sha256=manifest["binary_sha256"], source_commit=manifest["base_commit"],
                            provenance="modified", platform=manifest["target"],
                            source_archive_sha256=manifest["source_archive_sha256"],
                            instrumentation_sha256=manifest["instrumentation_sha256"],
                            comparison="explicit instrumented candidate, not an official release")
    return result


def quic_observation_coverage(output, mode):
    result = {}
    metadata = json.loads((output / "metadata.json").read_text())
    cases = metadata.get("diagnostic_cases", [])
    for role, name in (("attachment", "logs/attachment.log"), ("host", "target/logs/host.log")):
        records = []
        cap = False
        for line in (output / name).read_text().splitlines():
            if line.startswith("LOCHO_QUIC_STATS "):
                value = json.loads(line.removeprefix("LOCHO_QUIC_STATS "))
                require(value.get("schema") == 1 and value.get("role") == role, "invalid candidate QUIC schema/role")
                cap |= bool(value.get("detail_cap_reached"))
                if "elapsed_ms" in value:
                    records.append(value)
        identities = {record["connection_stable_id"] for record in records}
        epochs = []
        for record in records:
            if not epochs or record["connection_stable_id"] != epochs[-1][-1]["connection_stable_id"] or record["elapsed_ms"] < epochs[-1][-1]["elapsed_ms"]:
                epochs.append([])
            epochs[-1].append(record)
        # Restarting the attachment replaces the peer connection on BOTH ends;
        # the host process lifetime itself remains unchanged.
        expected_epochs = metadata.get("controller_exit", {}).get("generation", 1) if mode == "soak" else 1
        windows = []
        for case in cases:
            starts = [frame["large_started_controller_utc"] for frame in case["flows"] if "large_started_controller_utc" in frame]
            ends = [frame["large_finished_controller_utc"] for frame in case["flows"] if "large_finished_controller_utc" in frame]
            count = sum(min(starts) - 0.5 <= row["utc_ms"] / 1000 <= max(ends) + 0.5 for row in records) if starts and ends else 0
            windows.append(dict(case=case["case"], samples=count))
        if mode == "soak":
            # Retain timeout-time observation as well as a passing full phase.
            # The original soak owns an intentional attachment process restart;
            # IDs are process-local and may even be numerically reused afterward.
            for index, epoch in enumerate(epochs):
                windows.append(dict(case=f"process_epoch_{index + 1}", samples=len(epoch)))
            for line in (output / "operations.jsonl").read_text().splitlines():
                operation = json.loads(line)
                if operation.get("outcome") == "failed" and not operation.get("expected", False) and operation.get("telemetry"):
                    terminal = operation["utc"]
                    count = sum(terminal - 2 <= row["utc_ms"] / 1000 <= terminal + 1 for row in records)
                    windows.append(dict(case="unexpected_operation_terminal", combination=operation.get("combination"), samples=count))
        result[role] = dict(samples=len(records), stable_connections=len(identities), cap_reached=cap,
                            observed_connection_epochs=len(epochs), expected_connection_epochs=expected_epochs,
                            burst_windows=windows, path_samples=sum(bool(row.get("paths")) for row in records),
                            status="passed" if len(records) >= (1 if mode == "smoke" else 3)
                            and len(epochs) == expected_epochs and all(len(epoch) >= (1 if mode == "smoke" else 3) for epoch in epochs)
                            and not cap and all(window["samples"] > 0 for window in windows)
                            and any(row.get("paths") for row in records) else "not_exercised")
    return dict(result="passed" if all(value["status"] == "passed" for value in result.values()) else "failed",
                processes=result, note="UTC burst correlation has clock uncertainty; counters are connection/path-wide, not per-stream")


class ControlBridge:
    def __init__(self, control, run_id):
        self.control, self.run_id = control, run_id
        self.lock = asyncio.Lock()
        self.finished = False
        self.handlers = set()

    async def client(self, reader, writer):
        current = asyncio.current_task()
        self.handlers.add(current)
        try:
            while True:
                request = validate_message(await receive(reader), self.run_id)
                try:
                    async with self.lock:
                        if request["command"] == "stage":
                            if self.control.root is None:
                                await self.control.start()
                            elif self.control.broken or self.control.child.returncode is not None:
                                await self.control.reconnect()
                            result = dict(root=self.control.root)
                        else:
                            result = await self.control.rpc(request["command"], request["payload"], timeout=90)
                            if request["command"] == "finish":
                                await asyncio.wait_for(self.control.child.wait(), 5)
                                require(self.control.child.returncode == 0, "target removal confirmation failed")
                                self.finished = True
                    response = dict(version=1, run_id=self.run_id, id=request["id"], ok=True, payload=result)
                except Exception as error:
                    response = dict(version=1, run_id=self.run_id, id=request["id"], ok=False,
                                    error=type(error).__name__, payload={})
                writer.write(encode(response))
                await writer.drain()
        except (EOFError, asyncio.CancelledError):
            pass
        except Exception:
            # Never replay application operations; target watchdog owns cleanup.
            pass
        finally:
            writer.close()
            await writer.wait_closed()
            self.handlers.discard(current)

    async def close(self):
        for task in list(self.handlers):
            task.cancel()
        await asyncio.gather(*self.handlers, return_exceptions=True)
        await self.control.close()


async def stage_controller(args, run_id, binary, *, destination=None, parent=None):
    command = shlex.join([args.controller_python, "-c", STAGE_CONTROLLER])
    child = await spawn(destination if destination is not None else ssh_argv(args.controller_ssh, command))
    try:
        directory = Path(__file__).parent
        files = {name: base64.b64encode((directory / name).read_bytes()).decode()
                 for name in ("two_machine.py", "remote_runner.py", "native_support.py", "unix_streams.py")}
        child.stdin.write(encode(dict(files=files, parent=parent or args.controller_runtime_parent, run_id=run_id,
                                      binary_size=binary["path"].stat().st_size, binary_sha256=binary["sha256"])))
        await child.stdin.drain()
        with open(binary["path"], "rb") as source:
            while chunk := source.read(128 * 1024):
                child.stdin.write(chunk)
                await child.stdin.drain()
        child.stdin.close()
        receipt = await asyncio.wait_for(receive(child.stdout), 20)
        await asyncio.wait_for(child.wait(), 5)
        require(child.returncode == 0 and receipt.get("run_id") == run_id, "controller staging failed")
        return receipt["root"]
    finally:
        await reap(child)


async def collect_controller(args, root, run_id, destination):
    command = shlex.join([args.controller_python, "-c", COLLECT_CONTROLLER, root, run_id])
    child = await spawn(ssh_argv(args.controller_ssh, command))
    try:
        manifest = await receive(child.stdout)
        require(len(manifest["files"]) <= 64, "controller artifact manifest exceeds bound")
        total = 0
        for item in manifest["files"]:
            path = (destination / item["name"]).resolve()
            require(path.is_relative_to(destination) and type(item["size"]) is int
                    and 0 <= item["size"] <= 8 * 1024 * 1024, "unsafe controller artifact")
            total += item["size"]
            require(total <= 64 * 1024 * 1024, "controller artifact transfer exceeds bound")
            path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
            remaining, digest = item["size"], hashlib.sha256()
            with open(path, "xb", opener=lambda p, f: os.open(p, f, 0o600)) as output:
                while remaining:
                    data = await child.stdout.readexactly(min(remaining, 128 * 1024))
                    output.write(data)
                    digest.update(data)
                    remaining -= len(data)
            require(digest.hexdigest() == item["sha256"], "controller artifact transfer checksum mismatch")
        await asyncio.wait_for(child.wait(), 5)
        require(child.returncode == 0, "controller artifact transfer failed")
    finally:
        await reap(child)


def workload_options(args):
    options = []
    if getattr(args, "workload", "capacity") == "supported-admin":
        options += ["--workload", "supported-admin", "--concurrency", "4", "--levels", "4"]
    if args.relay_url:
        options += ["--relay-url", args.relay_url, "--relay-token-env", args.relay_token_env]
        if args.include_n0_relays:
            options += ["--include-n0-relays"]
        if args.disable_relay_qad:
            options += ["--disable-relay-qad"]
    for role in ("controller", "target"):
        path = getattr(args, f"{role}_relay_env")
        if path:
            options += [f"--{role}-relay-env", path]
    if getattr(args, "transfer_telemetry", False):
        options += ["--transfer-telemetry"]
    if getattr(args, "fixture_observation", False):
        options += ["--fixture-observation"]
    if getattr(args, "diagnostic_kind", "fragmentation") != "fragmentation":
        options += ["--diagnostic-kind", args.diagnostic_kind]
    if getattr(args, "no_conditioning_control", False):
        options += ["--no-conditioning-control"]
    return options


def phase_profile(args, mode):
    # Smoke remains a short prerequisite, never an acceptance certification.
    return "dev" if mode == "smoke" else getattr(args, "profile", "dev")


def relay_health(policy, expected_version):
    with urllib.request.urlopen(policy["url"] + "healthz", timeout=10) as response:
        body = response.read(16385)
    require(len(body) <= 16384, "relay health response exceeds bound")
    result = json.loads(body)
    require(result.get("status") == "ok" and result.get("version") == expected_version,
            "relay health/version preflight failed")
    return {key: result.get(key) for key in ("status", "version", "git_hash")}


async def run_one(args, mode, binaries):
    run_id = str(uuid.uuid4())
    options = workload_options(args)
    profile_name = phase_profile(args, mode)
    phase_args = workload_parser().parse_args([mode, "--profile", profile_name, *options])
    profile = schedule(phase_args)
    candidate = bool(getattr(args, "candidate_binary", None))
    binary_profile = "candidate" if candidate else "release"
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    output = Path(args.output).resolve() / f"two-machine-{mode}-{stamp}-{run_id}"
    args.latest_output = str(output)
    artifacts = Artifacts(output)
    runtime = Path(tempfile.mkdtemp(prefix="lp-", dir=args.runtime_parent)).resolve()
    runtime.chmod(0o700)
    socket = runtime / "c"
    require(len(os.fsencode(socket)) <= 103, "administrative socket path too long")
    target_args = workload_parser().parse_args([mode, "--profile", profile_name, "--binary-profile", binary_profile,
        "--controller-binary", "staged-on-controller", "--target-binary", args.target_binary,
        "--ssh-host", args.target_ssh_host, "--ssh-port", str(args.target_ssh_port), "--ssh-user", args.target_ssh_user,
        "--target-python", args.target_python, *options])
    control = SSHControl(target_args, run_id, profile)
    bridge = ControlBridge(control, run_id)
    server = await asyncio.start_unix_server(bridge.client, str(socket), limit=MESSAGE_LIMIT)
    root = None
    child = None
    errors = []
    result = None
    monitor = RelayMonitor(args, artifacts) if args.relay_monitor_ssh else None
    try:
        async with asyncio.timeout(profile["outer_budget"] + 60):
            if monitor:
                await monitor.start()
            root = await stage_controller(args, run_id, binaries["controller"])
            private_json(output / "administration.json", dict(run_id=run_id, controller_runtime=root,
                control="private Unix forwarding over independent verified SSH; no workload on administrator",
                target_binary_source=args.target_binary,
                target_binary_staging=getattr(args, "target_binary_staging", None),
                peer_lifetime=getattr(args, "peer_lifetime", None),
                relay_monitor_interval_seconds=getattr(args, "relay_monitor_interval", 5),
                binaries={role: {k: str(v) for k, v in info.items() if k != "path"} for role, info in binaries.items()},
                schedule=profile, relay_policy=relay_policy(args), relay_health=getattr(args, "relay_health", None),
                qad_verification="not_exercised; configuration and HTTPS health do not prove UDP reachability"))
            forwarded = str(Path(root) / "c")
            argv = [args.controller_python, str(Path(root) / "two_machine.py"), mode, "--profile", profile_name,
                "--run-id", run_id, "--control-socket", forwarded, "--controller-binary", str(Path(root) / "locho"),
                "--target-binary", args.target_binary, "--binary-profile", binary_profile,
                "--controller-provenance", binaries["controller"]["provenance"], "--target-provenance", binaries["target"]["provenance"],
                "--controller-source-commit", binaries["controller"]["source_commit"],
                "--target-source-commit", binaries["target"]["source_commit"],
                "--ssh-host", args.target_ssh_host, "--ssh-port", str(args.target_ssh_port), "--ssh-user", args.target_ssh_user,
                "--target-python", args.target_python, "--output", str(Path(root) / "artifacts"), *options,
                "--approve-execution"]
            child = await spawn(ssh_argv(args.controller_ssh, shlex.join(argv), f"{forwarded}:{socket}"))
            stdout, stderr = await child.communicate()
            require(len(stdout) <= MESSAGE_LIMIT and len(stderr) <= MESSAGE_LIMIT, "controller diagnostic output exceeds bound")
            if stdout.strip():
                result = json.loads(stdout.splitlines()[-1])
            artifacts.json("administration-result.json", dict(controller_exit=child.returncode,
                                target_runtime=control.root, result=result, stderr_bytes=len(stderr)))
            await collect_controller(args, root, run_id, output)
            if candidate:
                coverage = quic_observation_coverage(output, mode)
                artifacts.json("quic-observation-coverage.json", coverage)
                require(coverage["result"] == "passed", "candidate QUIC observation coverage incomplete")
            require(child.returncode == 0 and bridge.finished and result and result["result"] == "passed",
                    "remote workload or cleanup gates failed; inspect summary.json")
            require(not monitor or not monitor.error, "required relay metrics collection failed")
    except BaseException as error:
        errors.append(f"{type(error).__name__}: {error}"[:2048])
        if root and not (output / "summary.json").exists():
            try:
                async with asyncio.timeout(20):
                    await collect_controller(args, root, run_id, output)
            except Exception as failure:
                errors.append(f"artifact collection: {type(failure).__name__}")
    finally:
        if monitor:
            await monitor.close()
            if monitor.error and not any("relay metrics" in error for error in errors):
                errors.append("required relay metrics incomplete")
        await reap(child)
        if control.root and not bridge.finished:
            try:
                async with asyncio.timeout(30):
                    if control.broken or control.child.returncode is not None:
                        await control.reconnect()
                    receipt = await control.rpc("stop", timeout=20)
                    artifacts.json("administrative-target-cleanup.json", receipt)
            except Exception:
                errors.append("target cleanup unverified; independent watchdog remains authoritative")
        server.close()
        await server.wait_closed()
        await bridge.close()
        shutil.rmtree(runtime)
        # Retain remote controller diagnostics on failure; never remove an
        # unverified runtime simply because the local SSH client exited.
        if root and not errors:
            command = shlex.join([args.controller_python, "-c", REMOVE_CONTROLLER, root, run_id])
            removal = await spawn(ssh_argv(args.controller_ssh, command))
            try:
                await asyncio.wait_for(removal.communicate(), 10)
                require(removal.returncode == 0, "controller staging removal failed")
            except Exception:
                errors.append("controller staging removal unverified")
            finally:
                await reap(removal)
    artifacts.json("administration-summary.json", dict(run_id=run_id, result="failed" if errors else "passed",
                                                      errors=errors, target_runtime_removed=bridge.finished,
                                                      relay_metrics=dict(samples=monitor.samples, error=monitor.error) if monitor else None))
    print(json.dumps(dict(mode=mode, result="failed" if errors else "passed", artifacts=str(output))), flush=True)
    return not errors


async def main(args):
    runtime = Path(tempfile.mkdtemp(prefix="lb-", dir=args.runtime_parent)).resolve()
    runtime.chmod(0o700)
    target_stage = None
    target_stage_id = str(uuid.uuid4())
    target_args = workload_parser().parse_args(["smoke", "--controller-binary", "staged-on-controller",
        "--target-binary", args.target_binary or "privately-staged-release", "--ssh-host", args.target_ssh_host,
        "--ssh-port", str(args.target_ssh_port), "--ssh-user", args.target_ssh_user,
        "--target-python", args.target_python])
    target_control = SSHControl(target_args, target_stage_id, schedule(target_args))
    try:
        command = shlex.join([args.target_python, "-c", "import json,platform; print(json.dumps([platform.system(),platform.machine()]))"])
        probe = await spawn(target_control.ssh_argv(command))
        try:
            stdout, _ = await asyncio.wait_for(probe.communicate(), 15)
            require(probe.returncode == 0 and len(stdout) <= 4096, "target platform preflight failed")
            require(json.loads(stdout) == list(TARGET_PLATFORMS[args.target_platform]),
                    "target OS/architecture does not match selected release platform")
        finally:
            await reap(probe)
        policy = relay_policy(args)
        if policy:
            args.relay_health = await asyncio.to_thread(relay_health, policy, args.expected_relay_version)
        if getattr(args, "candidate_binary", None):
            binaries = await asyncio.to_thread(candidate_binaries, runtime, args.candidate_binary, args.candidate_manifest)
        else:
            binaries = await asyncio.to_thread(release_binaries, runtime, args.target_platform,
                                              getattr(args, "github_api_via_gh", False))
        if args.stage_target_binary:
            command = shlex.join([args.target_python, "-c", STAGE_CONTROLLER])
            target_stage = await stage_controller(args, target_stage_id, binaries["target"],
                destination=target_control.ssh_argv(command), parent=args.target_staging_parent)
            args.target_binary = str(Path(target_stage) / "locho")
            args.target_binary_staging = dict(root=target_stage, run_id=target_stage_id)
        else:
            require(binaries["target"]["sha256"] == args.target_sha256, "existing target binary does not match official archive")
        command = shlex.join([args.target_python, "-c",
            "import hashlib,pathlib,sys; p=pathlib.Path(sys.argv[1]); print(hashlib.sha256(p.read_bytes()).hexdigest())",
            args.target_binary])
        probe = await spawn(target_control.ssh_argv(command))
        try:
            stdout, _ = await asyncio.wait_for(probe.communicate(), 15)
            require(probe.returncode == 0 and len(stdout) <= 256
                    and stdout.decode().strip() == binaries["target"]["sha256"],
                    "measured target source binary does not match official archive")
        finally:
            await reap(probe)
        print(json.dumps(dict(event="binary_verification", binaries={role: {k: str(v) for k, v in item.items() if k != "path"}
                                                                    for role, item in binaries.items()})), flush=True)
        transport = args.mode == "diagnostic" and getattr(args, "diagnostic_kind", None) == "transport"
        modes = ["diagnostic", "diagnostic"] if transport else [args.mode] if args.mode != "both" else ["stress", "soak"]
        if policy and args.mode != "smoke":
            modes.insert(0, "smoke")
        failed = False
        lifetime = 0
        for mode in modes:
            if mode == "diagnostic":
                lifetime += 1
                args.peer_lifetime = lifetime
            if not await run_one(args, mode, binaries):
                failed = True
                if not (transport and mode == "diagnostic" and lifetime == 1
                        and transport_comparison_can_continue(Path(args.latest_output))):
                    return 1  # never continue after missing gates, cleanup or non-foreground failures
                print(json.dumps(dict(event="independent_peer_comparison", prior_lifetime=1,
                                      prior_result="failed", next_lifetime=2,
                                      note="predeclared fresh comparison; original failure retained")), flush=True)
        return 1 if failed else 0
    finally:
        try:
            if target_stage:
                command = shlex.join([args.target_python, "-c", REMOVE_CONTROLLER, target_stage, target_stage_id])
                removal = await spawn(target_control.ssh_argv(command))
                try:
                    await asyncio.wait_for(removal.communicate(), 10)
                    require(removal.returncode == 0, "private target binary staging removal failed")
                    print(json.dumps(dict(event="target_binary_staging_cleanup", root=target_stage,
                                          run_id=target_stage_id, removed=True)), flush=True)
                finally:
                    await reap(removal)
        finally:
            shutil.rmtree(runtime)


def transport_comparison_can_continue(output):
    """Only a complete foreground-only failed lifetime permits the second comparison."""
    try:
        summary = json.loads((output / "summary.json").read_text())
        metadata = json.loads((output / "metadata.json").read_text())
        admin = json.loads((output / "administration-summary.json").read_text())
        required = {"readiness", "same_lifetime_fd_attachment", "same_lifetime_fd_host", "diagnostic_resources_before",
                    "diagnostic_resources_after", "mixed_probe_observation", "mixed_overlap_coverage",
                    "burst_schedule_coverage", "resource_observation_coverage",
                    "persistent_probe_continuity", "application_quiet", "required_metrics", "forwarding_coverage",
                    "transport_observation", "custom_relay_observation", "artifact_transfer", "owned_process_cleanup"}
        required.add("no_conditioning_control" if metadata["review"]["schedule"].get("control_wait_seconds") == 5 else "conditioning")
        return (summary["error"] is None and not summary["cleanup_errors"]
                and summary["counts"].get("unexpected_failures", 0) > 0
                and not summary["counts"].get("validation_failures", 0)
                and all(summary["gates"].get(name, {}).get("status") == "passed" for name in required)
                and all(gate["status"] == "passed" for name, gate in summary["gates"].items()
                        if name not in ("diagnostic_cases", "integrity_and_steady_state"))
                and metadata["review"]["schedule"]["diagnostic_kind"] == "transport"
                and len(metadata["diagnostic_cases"]) == 3
                and admin["target_runtime_removed"]
                and admin["relay_metrics"]["samples"] >= 3 and not admin["relay_metrics"]["error"]
                and admin["errors"] == ["AssertionError: remote workload or cleanup gates failed; inspect summary.json"])
    except (OSError, ValueError, KeyError, TypeError):
        return False


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("mode", choices=("smoke", "stress", "soak", "diagnostic", "both"))
    cli.add_argument("--profile", choices=("dev", "acceptance"), default="dev")
    cli.add_argument("--workload", choices=("capacity", "supported-admin"), default="capacity",
                     help="explicit supported-admin contract or unchanged capacity workload")
    cli.add_argument("--controller-ssh", required=True)
    cli.add_argument("--controller-python", default="python3")
    cli.add_argument("--controller-runtime-parent", default="/var/tmp")
    cli.add_argument("--target-ssh-host", required=True)
    cli.add_argument("--target-ssh-port", type=int, default=2223)
    cli.add_argument("--target-ssh-user", required=True)
    cli.add_argument("--target-python", required=True)
    cli.add_argument("--target-platform", choices=tuple(TARGET_PLATFORMS), default="aarch64-apple-darwin")
    cli.add_argument("--target-binary")
    cli.add_argument("--target-sha256")
    cli.add_argument("--stage-target-binary", action="store_true",
                     help="privately stage the checksum-verified official target release; never install")
    cli.add_argument("--target-staging-parent", default="/var/tmp")
    cli.add_argument("--github-api-via-gh", action="store_true",
                     help="fetch release metadata using existing gh authentication; archive hashes remain verified")
    cli.add_argument("--candidate-binary", help="explicit local Linux x86_64 instrumented candidate, not an official release")
    cli.add_argument("--candidate-manifest", help="candidate binary/source/instrumentation SHA-256 provenance JSON")
    cli.add_argument("--runtime-parent", default=tempfile.gettempdir())
    cli.add_argument("--output", default="artifacts")
    cli.add_argument("--relay-url")
    cli.add_argument("--relay-token-env", default="LOCHO_RELAY_TOKEN")
    cli.add_argument("--include-n0-relays", action="store_true")
    cli.add_argument("--disable-relay-qad", action="store_true")
    cli.add_argument("--controller-relay-env")
    cli.add_argument("--target-relay-env")
    cli.add_argument("--expected-relay-version", default="1.4.1")
    cli.add_argument("--transfer-telemetry", action="store_true")
    cli.add_argument("--fixture-observation", action="store_true")
    cli.add_argument("--diagnostic-kind", choices=("fragmentation", "duplex", "mixed", "staggered", "transport"), default="fragmentation")
    cli.add_argument("--no-conditioning-control", action="store_true", help="transport diagnostic: two fresh lifetimes with 5s wait instead of conditioning transfers")
    cli.add_argument("--relay-monitor-ssh", help="approved read-only relay VM SSH route")
    cli.add_argument("--relay-monitor-key")
    cli.add_argument("--relay-monitor-host-key-alias")
    cli.add_argument("--relay-monitor-known-hosts")
    cli.add_argument("--relay-monitor-interval", type=float, default=5, help="bounded relay sampling pause, 1–30 seconds")
    cli.add_argument("--dry-run", action="store_true", help="offline review; no SSH, downloads, or files")
    cli.add_argument("--approve-execution", action="store_true")
    cli.add_argument("--approve-acceptance", action="store_true",
                     help="additional explicit approval for the 20m/1h acceptance workload")
    arguments = cli.parse_args()
    if bool(arguments.candidate_binary) != bool(arguments.candidate_manifest):
        cli.error("candidate binary and candidate manifest must be supplied together")
    candidate_scope = ((arguments.mode == "diagnostic" and arguments.diagnostic_kind == "transport" and arguments.no_conditioning_control)
                       or (arguments.mode == "soak" and arguments.profile == "dev" and not arguments.no_conditioning_control))
    if arguments.candidate_binary and not (candidate_scope and arguments.workload == "capacity" and arguments.stage_target_binary
            and arguments.target_platform == "x86_64-unknown-linux-gnu" and arguments.relay_monitor_ssh):
        cli.error("candidate requires privately staged monitored Linux transport control or bounded dev soak; never acceptance")
    if arguments.workload == "supported-admin" and arguments.mode == "diagnostic":
        cli.error("supported-admin is an acceptance/endurance workload, not a capacity diagnostic")
    if arguments.no_conditioning_control and not (arguments.mode == "diagnostic" and arguments.diagnostic_kind == "transport"):
        cli.error("no-conditioning control requires transport diagnostic mode")
    if not 1 <= arguments.relay_monitor_interval <= 30:
        cli.error("relay monitor interval must be 1–30 seconds")
    if arguments.mode == "diagnostic" and arguments.diagnostic_kind == "transport" and not arguments.relay_monitor_ssh:
        cli.error("transport comparison requires an approved relay monitor SSH route")
    if arguments.stage_target_binary:
        if arguments.target_binary or arguments.target_sha256:
            cli.error("private target staging cannot be combined with existing target binary arguments")
    elif not (arguments.target_binary and arguments.target_sha256):
        cli.error("supply --target-binary and --target-sha256, or explicitly select --stage-target-binary")
    if arguments.mode == "diagnostic" and arguments.profile != "dev":
        cli.error("diagnostic is a short development profile, not acceptance")
    policy = relay_policy(arguments)
    if policy and not (arguments.controller_relay_env and arguments.target_relay_env):
        cli.error("custom relay requires approved environment-file paths on both machines")
    if arguments.dry_run:
        phases = [arguments.mode] if arguments.mode != "both" else ["stress", "soak"]
        if arguments.mode == "diagnostic" and arguments.diagnostic_kind == "transport":
            phases = ["diagnostic", "diagnostic"]
        if policy and arguments.mode != "smoke":
            phases.insert(0, "smoke")
        print(json.dumps(dict(controller=arguments.controller_ssh, target=arguments.target_ssh_host,
                              workload=arguments.workload,
                              suite_role="supported_workload_acceptance" if arguments.workload == "supported-admin" else "capacity_characterization",
                              product_release_certification=False,
                              target_user=arguments.target_ssh_user, target_port=arguments.target_ssh_port,
                              target_platform=arguments.target_platform,
                              target_binary_policy="private checksum-verified candidate staging" if arguments.candidate_binary else
                                  "private verified release staging" if arguments.stage_target_binary else "copy existing verified release",
                              target_staging_parent=arguments.target_staging_parent if arguments.stage_target_binary else None,
                              release_metadata_source="explicit candidate manifest, not release metadata" if arguments.candidate_binary else
                                  "existing GitHub CLI authentication" if arguments.github_api_via_gh else "public GitHub HTTPS API",
                              binary_comparison="explicit modified QUIC-statistics candidate" if arguments.candidate_binary else "official release",
                              relay_monitor=dict(destination=arguments.relay_monitor_ssh, interval_seconds=arguments.relay_monitor_interval,
                                                 scope="whole relay, including other clients; read-only"),
                             relay_policy=policy, environment_sources=dict(controller=arguments.controller_relay_env,
                                                                          target=arguments.target_relay_env),
                             phases=[dict(mode=mode, profile=phase_profile(arguments, mode),
                                          schedule=schedule(workload_parser().parse_args([mode, "--profile", phase_profile(arguments, mode), *workload_options(arguments)])))
                                     for mode in phases], certification=False), indent=2))
        raise SystemExit(0)
    if not arguments.approve_execution:
        cli.error("explicit approval required for remote staging, load and isolated attachment restart")
    if arguments.profile == "acceptance" and not arguments.approve_acceptance:
        cli.error("long workloads require separately reviewed --approve-acceptance")
    raise SystemExit(asyncio.run(main(arguments)))
