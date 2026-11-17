"""Shared safety primitives for the opt-in two-machine native harness."""

import asyncio
import collections
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import resource
import sys
import time
import uuid

from urllib.parse import urlsplit

from unix_streams import HEADER, capture, quote, require, write_toml

PROTOCOL_VERSION = 1
MESSAGE_LIMIT = 2 * 1024 * 1024
LOG_LIMIT = 1024 * 1024
EVENT_LIMIT = 2000

# This guard is the actual parent of the binary. EOF on its private owner pipe
# survives controller/supervisor SIGKILL; only its retained child handle is ever
# signalled. No PID-file-based kill or global process-name matching is needed.
PROCESS_GUARD = r'''
import json,os,resource,selectors,signal,subprocess,sys,time
fd=int(sys.argv[1]); receipt=sys.argv[2]; budget=float(sys.argv[3])
resource.setrlimit(resource.RLIMIT_NOFILE,(int(sys.argv[4]),int(sys.argv[5])))
stopping=False
premature_exit=False
child=None
def stop(*_):
    global stopping,premature_exit
    if child is not None and child.poll() is not None: premature_exit=True
    stopping=True
signal.signal(signal.SIGTERM,stop); signal.signal(signal.SIGINT,stop)
child=subprocess.Popen(sys.argv[6:],stdin=subprocess.DEVNULL)
out=os.open(receipt,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
with os.fdopen(out,"w") as stream: json.dump({"pid":child.pid},stream)
selector=selectors.DefaultSelector(); selector.register(fd,selectors.EVENT_READ)
deadline=time.monotonic()+budget
while child.poll() is None and not stopping and time.monotonic()<deadline:
    for key,_ in selector.select(0.1):
        if not os.read(key.fd,1): stopping=True
if child.poll() is None:
    child.terminate()
    try: child.wait(timeout=3)
    except subprocess.TimeoutExpired:
        child.kill(); child.wait(timeout=1)
if premature_exit or (not stopping and child.returncode == 0): sys.exit(125)
sys.exit(child.returncode if child.returncode >= 0 else 128-child.returncode)
'''


def message(run_id, request_id, command, payload=None):
    return dict(version=PROTOCOL_VERSION, run_id=run_id, id=request_id,
                command=command, payload=payload or {})


def validate_message(value, run_id):
    require(isinstance(value, dict), "protocol message must be an object")
    require(type(value.get("version")) is int and value["version"] == PROTOCOL_VERSION,
            "unsupported protocol version")
    require(value.get("run_id") == run_id, "wrong run identity")
    require(type(value.get("id")) is int and value["id"] >= 0, "invalid request identity")
    require(isinstance(value.get("command"), str), "invalid command")
    require(isinstance(value.get("payload"), dict), "invalid payload")
    return value


def encode(value):
    data = json.dumps(value, separators=(",", ":")).encode() + b"\n"
    require(len(data) <= MESSAGE_LIMIT, "protocol message exceeds bound")
    return data


async def receive(reader):
    line = await reader.readline()
    if not line:
        raise EOFError("closed protocol stream")
    require(len(line) <= MESSAGE_LIMIT and line.endswith(b"\n"),
            "closed or oversized protocol stream")
    return json.loads(line)


def private_json(path, value):
    path = Path(path)
    temporary = path.with_suffix(path.suffix + ".new")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as output:
        json.dump(value, output)
    os.replace(temporary, path)


class Artifacts:
    def __init__(self, root):
        self.root = Path(root)
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        (self.root / "logs").mkdir(mode=0o700, exist_ok=True)
        self.secrets = set()
        self.runtime_paths = set()
        self.events = collections.Counter()
        self.truncated = collections.Counter()

    def redact(self, text, runtime=False):
        for secret in sorted(self.secrets, key=len, reverse=True):
            if secret:
                text = text.replace(secret, "<redacted>")
        if runtime:
            for path in self.runtime_paths:
                text = text.replace(path, "<runtime>")
        return text

    def sanitize(self, value):
        if isinstance(value, str):
            return self.redact(value)
        if isinstance(value, dict):
            return {key: self.sanitize(item) for key, item in value.items()}
        if isinstance(value, list):
            return [self.sanitize(item) for item in value]
        return value

    def json(self, name, value):
        private_json(self.root / name, self.sanitize(value))

    def event(self, name, value, cap=EVENT_LIMIT):
        if self.events[name] >= cap:
            self.truncated[name] += 1
            return
        self.events[name] += 1
        record = {"utc": time.time(), "monotonic": time.monotonic(), **value}
        with open(self.root / name, "a", opener=lambda p, f: os.open(p, f, 0o600)) as out:
            out.write(json.dumps(self.sanitize(record)) + "\n")


async def environment(root, relay_file=None, artifacts=None):
    # Never implicitly inherit production Locho configuration or test overrides.
    base = {k: v for k, v in os.environ.items() if not k.startswith("LOCHO_")}
    env = base
    if relay_file:
        original = Path(relay_file).expanduser()
        require(not original.is_symlink(), "relay environment symlink refused")
        path = original.resolve()
        require(path.is_file(), "approved relay environment file is missing")
        require(path.stat().st_uid == os.getuid() and path.stat().st_mode & 0o077 == 0,
                "approved relay environment file must be privately owned")
        require(path.parent.stat().st_uid == os.getuid() and path.parent.stat().st_mode & 0o077 == 0,
                "approved relay environment parent must be private")
        status, output = await capture(
            "/bin/sh", "-c", 'set -a; . "$1"; shift; exec "$@"', "relay-env",
            str(path), sys.executable, "-c", "import os,json; print(json.dumps(dict(os.environ)))",
            env=base, timeout=5)
        require(status == 0, "approved relay environment could not be sourced")
        env = json.loads(output)
        if artifacts:
            artifacts.secrets.update(v for k, v in env.items() if base.get(k) != v and v
                                     and k not in ("SHLVL", "PWD", "OLDPWD", "_"))
            for value in list(artifacts.secrets):
                try:
                    address = urlsplit(value)
                    artifacts.secrets.update(part for part in (address.username, address.password) if part)
                except ValueError:
                    pass
    env.update(LOCHO_STATE_DIR=str(Path(root) / "state"), NO_COLOR="1", RUST_LOG="info")
    return env


def normalize_relay_url(value):
    require(isinstance(value, str) and not any(c.isspace() for c in value), "invalid relay URL")
    try:
        parts = urlsplit(value)
        port = parts.port
        require(parts.scheme == "https" and parts.hostname and parts.username is None
                and parts.password is None and parts.path in ("", "/") and not parts.query and not parts.fragment,
                "relay must be a credential-free HTTPS base URL")
        require(port is None or 1 <= port <= 65535, "invalid relay port")
        host = parts.hostname.lower()
        if ":" in host:
            host = f"[{host}]"
        return f"https://{host}" + (f":{port}" if port and port != 443 else "") + "/"
    except ValueError:
        raise AssertionError("invalid relay URL") from None


def validate_relay_policy(value):
    if value is None:
        return None
    require(isinstance(value, dict) and set(value) == {"url", "token_env", "include_n0_relays", "quic_address_discovery"},
            "invalid relay policy fields")
    require(isinstance(value["token_env"], str) and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", value["token_env"]),
            "invalid relay token environment name")
    require(type(value["include_n0_relays"]) is bool and type(value["quic_address_discovery"]) is bool,
            "relay policy flags must be booleans")
    return value | {"url": normalize_relay_url(value["url"])}


def relay_policy(args):
    if not getattr(args, "relay_url", None):
        return None
    return validate_relay_policy(dict(url=args.relay_url, token_env=args.relay_token_env,
                                      include_n0_relays=args.include_n0_relays,
                                      quic_address_discovery=not args.disable_relay_qad))


def write_relay_config(root, policy, env):
    policy = validate_relay_policy(policy)
    if policy is None:
        return None
    token = env.get(policy["token_env"])
    require(isinstance(token, str) and 0 < len(token) <= 4096
            and all(33 <= ord(c) <= 126 for c in token), "required relay bearer token is missing or invalid")
    path = Path(root) / "relay.toml"
    write_toml(path, f'include_n0_relays = {str(policy["include_n0_relays"]).lower()}\n\n[[relays]]\n'
               f'url = {quote(policy["url"])}\ntoken_env = {quote(policy["token_env"])}\n'
               f'quic_address_discovery = {str(policy["quic_address_discovery"]).lower()}\n')
    return path


def relay_arguments(path):
    return ["--relay-config", str(path)] if path else []


def custom_relay_observed(history, policy):
    expected = validate_relay_policy(policy)["url"]
    for record in history:
        for value in re.findall(r"relay\((https://[^)]+)\)", record["path"]):
            if normalize_relay_url(value) == expected:
                return True
    return False


class TransferProgress:
    """Observe existing I/O calls without changing frame reads or chunk sizes.

    Queued/drained bytes describe local flow control, not remote acknowledgements.
    A cancelled readexactly leaves its partial data in StreamReader's buffer;
    report that optional buffer observation separately from consumed bytes.
    """

    def __init__(self, size):
        self.started = time.monotonic()
        self.size = size
        self.queued = self.drained = self.received = 0
        self.header_read = self.payload_read = self.drain_calls = 0
        self.read_phase = self.write_phase = self.socket_phase = "not_started"
        self.read_complete = self.write_complete = False
        self.frame_verified = False
        self.times = collections.Counter()
        self.reader = self.writer = None

    def bind(self, reader, writer):
        self.reader, self.writer = reader, writer
        self.socket_phase = "connected"
        return ObservedReader(reader, self), ObservedWriter(writer, self)

    def snapshot(self):
        buffer = getattr(self.reader, "_buffer", None)
        buffered = len(buffer) if isinstance(buffer, (bytes, bytearray)) else None
        transport = getattr(self.writer, "transport", None)
        try:
            queued_buffer = transport.get_write_buffer_size() if transport else None
        except (AttributeError, RuntimeError):
            queued_buffer = None
        return dict(elapsed_seconds=time.monotonic() - self.started, expected_payload_bytes=self.size,
                    socket_phase=self.socket_phase, write_phase=self.write_phase, read_phase=self.read_phase,
                    write_wait_completed=self.write_complete, read_wait_completed=self.read_complete,
                    request_header_queued=min(self.queued, HEADER.size), request_payload_queued=max(0, self.queued - HEADER.size),
                    local_drain_completed_bytes=self.drained, local_write_buffer_bytes=queued_buffer,
                    response_header_consumed=self.header_read, response_payload_consumed=self.payload_read,
                    response_consumed_bytes=self.received, buffered_response_bytes=buffered,
                    response_arrived_lower_bound=self.received + (buffered or 0),
                    frame_verified=self.frame_verified, drain_calls=self.drain_calls, phase_seconds=dict(self.times),
                    observation_scope="local I/O; drain is not a remote acknowledgement; buffer is optional non-destructive observation")


class ObservedReader:
    def __init__(self, reader, progress):
        self.reader, self.progress = reader, progress
        self.calls = 0

    async def readexactly(self, size):
        phase = "response_header" if self.calls == 0 else "response_payload"
        self.calls += 1
        p = self.progress
        p.read_phase, p.read_complete = phase, False
        began = time.monotonic()
        try:
            data = await self.reader.readexactly(size)
            p.received += len(data)
            if phase == "response_header":
                p.header_read += len(data)
            else:
                p.payload_read += len(data)
            p.read_complete = True
            return data
        except asyncio.IncompleteReadError as error:
            p.received += len(error.partial)
            if phase == "response_header":
                p.header_read += len(error.partial)
            else:
                p.payload_read += len(error.partial)
            raise
        finally:
            p.times[phase] += time.monotonic() - began

    async def read(self, size=-1):
        p = self.progress
        p.read_phase, p.read_complete = "response_eof", False
        began = time.monotonic()
        try:
            data = await self.reader.read(size)
            p.received += len(data)
            p.read_complete = True
            return data
        finally:
            p.times["response_eof"] += time.monotonic() - began

    def __getattr__(self, name):
        return getattr(self.reader, name)


class ObservedWriter:
    def __init__(self, writer, progress):
        self.writer, self.progress = writer, progress

    def write(self, data):
        self.progress.write_phase = "request_write"
        self.writer.write(data)
        self.progress.queued += len(data)

    async def drain(self):
        p = self.progress
        p.write_phase, p.write_complete = "request_drain", False
        p.drain_calls += 1
        began = time.monotonic()
        try:
            await self.writer.drain()
            p.drained = p.queued
            p.write_complete = True
        finally:
            p.times["request_drain"] += time.monotonic() - began

    def write_eof(self):
        self.progress.write_phase = "request_eof"
        self.writer.write_eof()

    def close(self):
        self.progress.socket_phase = "closing"
        self.writer.close()

    async def wait_closed(self):
        began = time.monotonic()
        try:
            await self.writer.wait_closed()
            self.progress.socket_phase = "closed"
        finally:
            self.progress.times["socket_close"] += time.monotonic() - began

    def __getattr__(self, name):
        return getattr(self.writer, name)


async def process_identity(pid):
    status, output = await capture("ps", "-p", str(pid), "-o", "lstart=", timeout=3)
    require(status == 0 and output.strip(), "process identity unavailable")
    return output.decode().strip()


def linux_resource_observation(pid, previous=None, *, proc_root=Path("/proc"), cgroup_root=Path("/sys/fs/cgroup")):
    """Bounded numeric observations of an owned process; never retain socket paths.

    Cgroup values cover other processes too; namespace-hidden ancestors cannot be
    observed. Optional failures are explicit, not evidence of zero pressure.
    """
    now = time.monotonic()
    result = dict(sample_monotonic=now, scope="owned PID; cgroups shared and visible ancestors only",
                  unavailable=[], cgroups=[], loopback_tcp_sockets=[], unix_socket_states=[])
    process = proc_root / str(pid)

    def bounded(path, limit=65536):
        with open(path) as source:
            value = source.read(limit + 1)
        require(len(value) <= limit, "resource observation input exceeds bound")
        return value

    def counters(text):
        return {parts[0]: int(parts[1]) for line in text.splitlines()
                if len(parts := line.split()) == 2 and parts[1].isdigit()}

    try:
        fields = bounded(process / "stat").rsplit(")", 1)[1].split()
        result.update(process_start_ticks=int(fields[19]),
                      cpu_seconds=(int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"))
        if previous and previous.get("process_start_ticks") == result["process_start_ticks"]:
            elapsed = now - previous["sample_monotonic"]
            result.update(interval_seconds=elapsed,
                          interval_cpu_percent=100 * (result["cpu_seconds"] - previous["cpu_seconds"]) / elapsed)
    except (OSError, ValueError, IndexError, AssertionError):
        result["unavailable"].append("process_cpu")
    try:
        relative = next(line.split(":", 2)[2] for line in bounded(process / "cgroup").splitlines()
                        if line.startswith("0::"))
        root = cgroup_root.resolve()
        current = (root / relative.lstrip("/")).resolve()
        require(current.is_relative_to(root), "cgroup outside visible hierarchy")
        for _ in range(16):
            entry = dict(path="/" + str(current.relative_to(root)).removeprefix("."), unavailable=[])
            for name in ("cpu.max", "cpu.stat", "cpu.pressure", "memory.max", "memory.events"):
                try:
                    text = bounded(current / name, 8192)
                    if name in ("cpu.stat", "memory.events"):
                        entry[name] = counters(text)
                    elif name == "cpu.pressure":
                        entry[name] = {parts[0]: {key: float(value) for key, value in
                            (item.split("=", 1) for item in parts[1:])}
                            for line in text.splitlines() if (parts := line.split())}
                    else:
                        entry[name] = [None if item == "max" else int(item) for item in text.split()]
                except (OSError, ValueError, KeyError, AssertionError):
                    entry["unavailable"].append(name)
            old = next((item for item in (previous or {}).get("cgroups", []) if item["path"] == entry["path"]), {})
            for name in ("cpu.stat", "memory.events"):
                if name in entry and name in old:
                    entry[name + "_delta"] = {key: value - old[name][key] for key, value in entry[name].items()
                                               if key in old[name] and value >= old[name][key]}
            if "cpu.pressure" in entry and "cpu.pressure" in old:
                entry["pressure_total_usec_delta"] = {key: values["total"] - old["cpu.pressure"][key]["total"]
                    for key, values in entry["cpu.pressure"].items() if key in old["cpu.pressure"]}
            result["cgroups"].append(entry)
            if current == root:
                break
            current = current.parent
        else:
            result["unavailable"].append("cgroup_ancestors_beyond_16")
    except (OSError, ValueError, StopIteration, AssertionError):
        result["unavailable"].append("cgroup_v2_hierarchy")
    try:
        inodes = set()
        for index, fd in enumerate((process / "fd").iterdir()):
            require(index < 4096, "socket descriptor observation exceeds bound")
            try:
                link = os.readlink(fd)
            except OSError:
                continue  # FD may close during the snapshot.
            if link.startswith("socket:["):
                inodes.add(link[8:-1])
        for name in ("tcp", "tcp6"):
            # /proc tables include other processes; retain only owned socket inodes.
            text = bounded(process / "net" / name, 2 * 1024 * 1024)
            for line in text.splitlines()[1:]:
                fields = line.split()
                if len(fields) < 10 or fields[9] not in inodes:
                    continue
                local, peer = fields[1].split(":"), fields[2].split(":")
                loopback = (local[0].endswith("7F") and peer[0].endswith("7F")) if name == "tcp" else (
                    local[0] == peer[0] == "00000000000000000000000001000000")
                if loopback:
                    send, receive = (int(value, 16) for value in fields[4].split(":"))
                    result["loopback_tcp_sockets"].append(dict(inode=int(fields[9]), state=int(fields[3], 16),
                        local_port=int(local[1], 16), peer_port=int(peer[1], 16),
                        send_queue_bytes=send, receive_queue_bytes=receive))
        result["tcp_socket_details_dropped"] = max(0, len(result["loopback_tcp_sockets"]) - 128)
        result["loopback_tcp_sockets"] = result["loopback_tcp_sockets"][:128]
        try:
            for line in bounded(process / "net/unix", 2 * 1024 * 1024).splitlines()[1:]:
                fields = line.split()
                if len(fields) >= 7 and fields[6] in inodes:
                    result["unix_socket_states"].append(dict(inode=int(fields[6]), state=int(fields[5], 16),
                                                            socket_type=int(fields[4], 16)))
            result["unix_socket_details_dropped"] = max(0, len(result["unix_socket_states"]) - 128)
            result["unix_socket_states"] = result["unix_socket_states"][:128]
        except (OSError, ValueError, AssertionError):
            result["unavailable"].append("owned_unix_socket_states")
        result["unavailable"].append("unix_socket_queue_bytes_not_exposed_by_proc")
    except (OSError, ValueError, AssertionError):
        result["unavailable"].append("owned_loopback_socket_queues")
    return result


class OwnedProcess:
    """Child handle is authoritative; never signal an arbitrary recorded PID."""

    def __init__(self, name, argv, artifacts, env=None, soft=1024, hard=4096, budget=3755):
        self.name, self.argv, self.artifacts, self.env = name, argv, artifacts, env
        self.soft, self.hard = soft, hard
        self.budget = budget
        self.absolute_deadline = None
        self.owner_fd = None
        self.pid = None
        self.child = None
        self.generation = 0
        self.start_identity = None
        self.started = None
        self.lines = collections.deque(maxlen=128)
        self.paths = collections.deque(maxlen=512)
        self.categories = collections.Counter()
        self.pumps = []
        self.log_bytes = 0
        self.log_truncated = False
        self.fd_peak = 0
        self.resource_observation = False
        self.previous_resource_observation = None

    async def start(self):
        require(self.child is None or self.child.returncode is not None, "owned process already running")
        if self.absolute_deadline is None:
            self.absolute_deadline = time.monotonic() + self.budget
        remaining = self.absolute_deadline - time.monotonic()
        require(remaining > 0, "owned process hard deadline elapsed")
        read_fd, self.owner_fd = os.pipe()
        receipt = self.artifacts.root / f".{self.name}-{uuid.uuid4().hex}.pid"
        spawning = asyncio.create_task(asyncio.create_subprocess_exec(
            sys.executable, "-c", PROCESS_GUARD, str(read_fd), str(receipt), str(remaining),
            str(self.soft), str(self.hard), *map(str, self.argv), pass_fds=(read_fd,),
            env=self.env, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
            limit=65536, start_new_session=True))
        try:
            self.child = await asyncio.shield(spawning)
        except BaseException:
            if self.owner_fd is not None:
                os.close(self.owner_fd)
                self.owner_fd = None
            self.child = await asyncio.wait_for(spawning, 5)
            await asyncio.wait_for(self.child.wait(), 5)
            if receipt.exists():
                receipt.unlink()
            raise
        finally:
            os.close(read_fd)
        self.generation += 1
        self.lines.clear()
        self.started = time.monotonic()
        self.pumps = [asyncio.create_task(self.pump(stream))
                      for stream in (self.child.stdout, self.child.stderr)]
        try:
            async with asyncio.timeout(3):
                while not receipt.exists():
                    require(self.child.returncode is None, "process guard failed to start")
                    await asyncio.sleep(0.01)
                # Creation and write may race; only consume complete JSON.
                while True:
                    try:
                        self.pid = json.loads(receipt.read_text())["pid"]
                        break
                    except json.JSONDecodeError:
                        await asyncio.sleep(0.01)
            self.start_identity = await process_identity(self.pid)
        finally:
            if receipt.exists():
                receipt.unlink()
        return self

    async def pump(self, stream):
        # Read chunks instead of readline: arbitrarily long log lines cannot kill
        # the pump. Retain at most one bounded incomplete line for categorization.
        pending = ""
        while chunk := await stream.read(4096):
            text = chunk.decode(errors="replace")
            pending += text
            parts = pending.split("\n")
            if len(parts[-1]) > 8192:
                self.log_truncated = True
            pending = parts.pop()[-8192:]
            for line in parts:
                safe = self.artifacts.redact(line, runtime=True)
                self.lines.append(safe)
                for category in ("ERROR", "WARN", "panic", "SCNetworkInterfaceGetLocalizedDisplayName"):
                    if category in safe:
                        self.categories[category] += 1
                if "rate-limiting this endpoint" in safe or "traffic is being throttled" in safe:
                    self.categories["relay_rate_limited"] += 1
                if "transport path:" in safe:
                    observation = dict(utc=time.time(), generation=self.generation,
                                       path=safe.split("transport path:", 1)[1].strip())
                    self.paths.append(observation)
                    self.categories["transport_observations"] += 1
                    self.artifacts.event("transport.jsonl", dict(process=self.name, **observation))
            # Redact whole lines so credentials split across read chunks are safe.
            if parts:
                safe = self.artifacts.redact("\n".join(parts) + "\n", runtime=True).encode()
                remaining = LOG_LIMIT - self.log_bytes
                if remaining > 0:
                    with open(self.artifacts.root / "logs" / f"{self.name}.log", "ab",
                              opener=lambda p, f: os.open(p, f, 0o600)) as output:
                        output.write(safe[:remaining])
                    self.log_bytes += min(remaining, len(safe))
                self.log_truncated |= len(safe) > max(remaining, 0)
        if pending:
            self.lines.append(self.artifacts.redact(pending, runtime=True))

    async def ready(self, marker, timeout=30):
        async with asyncio.timeout(timeout):
            while not any(marker in line for line in self.lines):
                require(self.child.returncode is None, f"{self.name} exited before readiness")
                for pump in self.pumps:
                    if pump.done():
                        pump.result()
                await asyncio.sleep(0.05)

    async def stop(self, timeout=5, force_timeout=5):
        if self.child is None:
            return
        if self.child.returncode is None:
            # Retained live child handle prevents PID recycling until reaping.
            # Signal only the guard's live retained child handle. The guard owns
            # the Locho/fixture handle and applies its own bounded shutdown.
            self.child.terminate()
            try:
                await asyncio.wait_for(self.child.wait(), timeout)
            except asyncio.TimeoutError:
                if self.owner_fd is not None:
                    os.close(self.owner_fd)
                    self.owner_fd = None
                await asyncio.wait_for(self.child.wait(), force_timeout)
        try:
            results = await asyncio.wait_for(asyncio.gather(*self.pumps, return_exceptions=True), 2)
            for result in results:
                if isinstance(result, BaseException):
                    raise result
        finally:
            if self.owner_fd is not None:
                os.close(self.owner_fd)
                self.owner_fd = None
            for pump in self.pumps:
                if not pump.done():
                    pump.cancel()

    async def force_reap(self):
        if self.owner_fd is not None:
            os.close(self.owner_fd)
            self.owner_fd = None
        if self.child and self.child.returncode is None:
            self.child.terminate()
            await asyncio.wait_for(self.child.wait(), 5)
        for pump in self.pumps:
            pump.cancel()
        await asyncio.gather(*self.pumps, return_exceptions=True)

    async def sample(self):
        require(self.child and self.child.returncode is None, f"{self.name} exited unexpectedly")
        require(await process_identity(self.pid) == self.start_identity, "process lifetime changed")
        for pump in self.pumps:
            if pump.done():
                pump.result()
        status, output = await capture("ps", "-p", str(self.pid), "-o", "rss=,pcpu=", timeout=3)
        fields = output.decode().split()
        require(status == 0 and len(fields) == 2, "required process metrics unavailable")
        active = None
        endpoints = []
        if sys.platform.startswith("linux"):
            fds = len(list(Path(f"/proc/{self.pid}/fd").iterdir()))
        else:
            status, output = await capture("lsof", "-n", "-P", "-p", str(self.pid), "-FftnT", timeout=3)
            require(status == 0, "required descriptor metrics unavailable")
            lines = output.decode().splitlines()
            fds = sum(line[1:].isdigit() for line in lines if line.startswith("f"))
            active = sum(line == "TST=ESTABLISHED" for line in lines)
            socket_entry = False
            for line in lines:
                if line.startswith("f"):
                    socket_entry = False
                if line in ("tIPv4", "tIPv6", "tunix"):
                    socket_entry = True
                if socket_entry and line.startswith("n"):
                    endpoints.append(line[1:])
        self.fd_peak = max(self.fd_peak, fds)
        require(fds >= 3, "numeric descriptor metrics are incomplete")
        result = dict(process=self.name, pid=self.pid, guard_pid=self.child.pid, generation=self.generation,
                    start_identity=self.start_identity, uptime=time.monotonic() - self.started,
                    exit_status=self.child.returncode, rss_kib=int(fields[0]), cpu_percent=float(fields[1]),
                    fds=fds, fd_peak=self.fd_peak, active_connections=active,
                    endpoints=endpoints, transport=list(self.paths)[-1:], log_categories=dict(self.categories),
                     log_truncated=self.log_truncated)
        if self.resource_observation:
            if sys.platform.startswith("linux"):
                observed = linux_resource_observation(self.pid, self.previous_resource_observation)
                self.previous_resource_observation = observed
                result["resource_observation"] = observed
            else:
                result["resource_observation"] = {"unavailable": ["Linux proc/cgroup observations on this platform"]}
        return result


def capacity(root, soft, hard):
    import shutil
    inherited = resource.getrlimit(resource.RLIMIT_NOFILE)
    require(inherited[1] == resource.RLIM_INFINITY or hard <= inherited[1],
            "requested child hard descriptor limit exceeds inherited capacity")
    free = shutil.disk_usage(root).free
    require(free >= 256 * 1024 * 1024, "at least 256 MiB free runtime/artifact space required")
    return dict(cpu_count=os.cpu_count(), load_average=list(os.getloadavg()), free_disk_bytes=free,
                inherited_fd_limits=list(inherited), requested_fd_limits=[soft, hard],
                shared_resource_warning="CPU, memory and bandwidth are not isolated; review capacity before load")


def same_lifetime_growth(before, after, allowance):
    require(all(before[k] == after[k] for k in ("pid", "generation", "start_identity")),
            "descriptor gate must compare the same process lifetime")
    require(type(before.get("fds")) is int and type(after.get("fds")) is int,
            "required descriptor metrics missing")
    growth = after["fds"] - before["fds"]
    require(growth <= allowance, "same-lifetime descriptor growth exceeded allowance")
    return growth


def binary_metadata(path, provenance, commit):
    return dict(sha256=hashlib.sha256(Path(path).read_bytes()).hexdigest(),
                provenance=provenance, source_commit=commit)
