#!/usr/bin/env python3
"""Run the public Dart tunnel integration test against the reference API/real host.

Usage: python3 scripts/flutter-device-smoke.py <device-id> [--release]
Requires a compiled sibling examples API, release Locho binary, Flutter, and LAN
UDP reachability from the device. Override LOCHO_HOST_IP/LOCHO_EXAMPLES_DIR as needed.
Credentials live only in a temporary mode-0600 dart-define file, never argv/logs.
"""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import queue
import time
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
EXAMPLES = Path(os.environ.get("LOCHO_EXAMPLES_DIR", ROOT.parent / "locho_sdk_examples"))
DEVICE = sys.argv[1]

def port(kind):
    with socket.socket(socket.AF_INET, kind) as reservation:
        reservation.bind(("0.0.0.0", 0))
        return reservation.getsockname()[1]

def host_ip():
    if os.environ.get("LOCHO_HOST_IP"):
        return os.environ["LOCHO_HOST_IP"]
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.connect(("8.8.8.8", 80))
        return sock.getsockname()[0]

processes = []
with tempfile.TemporaryDirectory(prefix="flutter-smoke-", dir=ROOT / "target") as directory:
    directory = Path(directory)
    try:
        api_port = port(socket.SOCK_STREAM)
        environment = {**os.environ, "API_PORT": str(api_port), "API_DATABASE": str(directory / "api.sqlite")}
        log = (directory / "api.log").open("w")
        api = subprocess.Popen(["node", "dist/src/server.js"], cwd=EXAMPLES / "apps/api",
                               env=environment, stdout=log, stderr=log)
        processes.append(api)
        for attempt in range(100):
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{api_port}/health", timeout=1):
                    break
            except OSError:
                if api.poll() is not None:
                    raise RuntimeError("Reference API failed to start; run npm run build")
                time.sleep(0.1)
        else:
            raise RuntimeError("Reference API startup timeout")
        direct = f"{host_ip()}:{port(socket.SOCK_DGRAM)}"
        config = directory / "locho.toml"
        config.write_text(f'[[services]]\nname = "sample-api"\ntype = "tcp"\nendpoint = "127.0.0.1:{api_port}"\n')
        environment = {**os.environ, "LOCHO_STATE_DIR": str(directory / "state")}
        binary = ROOT / "target/release/locho"
        host = subprocess.Popen([str(binary), "host", "--config", str(config), "--bind-address", direct],
                                env=environment, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        processes.append(host)
        ready = queue.Queue()
        def watch_host():
            for line in host.stdout:
                if "locho direct-address" in line:
                    ready.put(True)
                    return
            ready.put(False)
        threading.Thread(target=watch_host, daemon=True).start()
        if not ready.get(timeout=15):
            raise RuntimeError("Locho host startup failed")
        share = subprocess.check_output([str(binary), "share", "sample-api", "--config", str(config)],
                                        env=environment, text=True).split()
        defines = directory / "defines.json"
        ticket = subprocess.check_output([str(binary), "share", "sample-api", "--config", str(config), "--ticket"], env=environment, text=True).strip()
        defines.touch(mode=0o600)
        defines.write_text(json.dumps({"LOCHO_HOST_ID": share[2], "LOCHO_CAPABILITY": share[3],
                                     "LOCHO_TICKET": ticket,
                                     "LOCHO_DIRECT_ADDRESS": "" if "--discovery" in sys.argv else direct}))
        command = ["flutter", "test", "integration_test/tunnel_test.dart", "-d", DEVICE,
                   "--dart-define-from-file=" + str(defines)]
        if "--release" in sys.argv:
            command = ["flutter", "build", "apk", "--target=lib/release_smoke.dart",
                       "--target-platform=android-arm64", "--dart-define-from-file=" + str(defines), "--release"]
        subprocess.run(command, cwd=ROOT / "packages/locho_flutter/example", check=True, timeout=600)
        if "--release" in sys.argv:
            adb = str(Path(os.environ.get("ANDROID_HOME", Path.home() / "Library/Android/sdk")) / "platform-tools/adb")
            def android(*args):
                return subprocess.check_output([adb, "-s", DEVICE, *args], text=True)
            android("install", "-r", str(ROOT / "packages/locho_flutter/example/build/app/outputs/flutter-apk/app-release.apk"))
            package = "dev.chop.locho.locho_flutter_example"
            android("shell", "am", "force-stop", package)
            android("logcat", "-c")
            android("shell", "am", "start", "-n", package + "/.MainActivity")
            for _ in range(90):
                time.sleep(1)
                pid = android("shell", "pidof", package).strip()
                if not pid:
                    raise RuntimeError("Release smoke app exited before reporting its result")
                output = android("logcat", "-d", "--pid=" + pid, "-s", "flutter:I")
                if "LOCHO_RELEASE_SMOKE_FAIL" in output:
                    raise RuntimeError("Release device smoke failed")
                if "LOCHO_RELEASE_SMOKE_PASS" in output:
                    print(f"Android release attach/health/close smoke passed on {DEVICE}.")
                    break
            else:
                raise RuntimeError("Release device smoke timeout")
            android("shell", "am", "force-stop", package)
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=12)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
