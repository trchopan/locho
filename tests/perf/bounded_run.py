#!/usr/bin/env python3
"""Run one reviewed command in a detached, bounded coordinator session.

The invoking process waits for completion, but the detached supervisor and its
private logs do not depend on its terminal or OpenCode lifetime. Remote workload
watchdogs remain necessary if the administrator loses power or connectivity.
"""

import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import uuid

from native_support import private_json


def stop(child):
    if child is not None and child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=60)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=5)


def supervise(args):
    root = Path(args.output).resolve()
    owner = json.loads((root / "owner.json").read_text())
    if owner != dict(run_id=args.run_id, command=args.command, timeout=args.timeout):
        raise RuntimeError("coordinator ownership/settings mismatch")
    signal.signal(signal.SIGHUP, signal.SIG_IGN)

    def interrupted(*_):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, interrupted)
    child = assertion = None
    started = time.monotonic()
    status = dict(run_id=args.run_id, supervisor_pid=os.getpid(), result="running", timeout=args.timeout,
                  sleep_assertion="not_requested", started_utc=time.time())
    try:
        if sys.platform == "darwin":
            assertion = subprocess.Popen(["/usr/bin/caffeinate", "-i", "-s", "-t", str(args.timeout + 90),
                                          "-w", str(os.getpid())], stdin=subprocess.DEVNULL,
                                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            status["sleep_assertion"] = dict(pid=assertion.pid,
                policy="prevent idle sleep on AC or battery; additionally prevent system sleep on AC; no global settings",
                ac_required=False,
                limitations="lid-close, forced sleep, shutdown, battery exhaustion and connectivity loss can interrupt control")
        child = subprocess.Popen(args.command, stdin=subprocess.DEVNULL, start_new_session=True)
        status["launcher_pid"] = child.pid
        private_json(root / "status.json", status)
        code = child.wait(timeout=args.timeout)
        status.update(result="passed" if code == 0 else "failed", exit_code=code)
    except subprocess.TimeoutExpired:
        status.update(result="failed", error="coordinator deadline exceeded")
    except KeyboardInterrupt:
        status.update(result="failed", error="coordinator interrupted")
    except Exception as error:
        status.update(result="failed", error_type=type(error).__name__)
    finally:
        # A second signal must not interrupt the bounded owned-child cleanup.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        try:
            stop(child)
            if child is not None:
                status["exit_code"] = child.returncode
        finally:
            stop(assertion)
            status.update(elapsed_seconds=time.monotonic() - started, finished_utc=time.time())
            private_json(root / "status.json", status)
    return 0 if status["result"] == "passed" else 1


def main(args):
    if args.supervise:
        return supervise(args)
    root = Path(args.output).resolve()
    root.mkdir(mode=0o700, parents=True, exist_ok=False)
    args.run_id = str(uuid.uuid4())
    private_json(root / "owner.json", dict(run_id=args.run_id, command=args.command, timeout=args.timeout))
    argv = [sys.executable, "-B", str(Path(__file__).resolve()), "--supervise", "--run-id", args.run_id,
            "--output", str(root), "--timeout", str(args.timeout), "--", *args.command]
    with open(root / "launcher.log", "xb", opener=lambda p, f: os.open(p, f, 0o600)) as log:
        child = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                 start_new_session=True)
    print(json.dumps(dict(event="coordinator_started", run_id=args.run_id, supervisor_pid=child.pid,
                          artifacts=str(root), timeout=args.timeout)), flush=True)
    try:
        code = child.wait()
    except KeyboardInterrupt:
        stop(child)
        code = 1
    print(json.dumps(dict(event="coordinator_finished", result="passed" if code == 0 else "failed",
                          artifacts=str(root))), flush=True)
    return code


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("--output", required=True, help="fresh private coordinator artifact directory")
    cli.add_argument("--timeout", type=int, required=True, help="hard command budget; 1–7200 seconds")
    cli.add_argument("--supervise", action="store_true", help=argparse.SUPPRESS)
    cli.add_argument("--run-id", help=argparse.SUPPRESS)
    cli.add_argument("command", nargs=argparse.REMAINDER)
    args = cli.parse_args()
    if args.command and args.command[0] == "--":
        args.command.pop(0)
    if not args.command or not 1 <= args.timeout <= 7200:
        cli.error("a command and a bounded timeout between 1 and 7200 seconds are required")
    raise SystemExit(main(args))
