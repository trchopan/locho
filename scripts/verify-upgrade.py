#!/usr/bin/env python3
"""Rehearse an executable-only Locho upgrade using disposable loopback fixtures."""
import argparse
import base64
from contextlib import ExitStack, contextmanager
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import socket
import socketserver
import ssl
import subprocess
import tempfile
import threading
import time
import tomllib


class RehearsalFailure(Exception):
    """Only fixed, credential-free failure descriptions reach the console."""


def require(condition, message):
    if not condition:
        raise RehearsalFailure(message)


def run(command, env=None):
    result = subprocess.run(command, env=env, capture_output=True, timeout=30)
    require(result.returncode == 0, "Fixture CLI command failed")
    return result.stdout.decode().strip()


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


class HttpFixture(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"locho-upgrade-http"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


class EchoFixture(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(15)
        try:
            while data := self.request.recv(4096):
                self.request.sendall(data)
        except OSError:
            pass


class EchoServer(socketserver.ThreadingTCPServer):
    daemon_threads = True


@contextmanager
def serving(server):
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server.server_address[1]
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


class Process:
    def __init__(self, binary, args, env, directory, number):
        self.path = directory / f"process-{number}.log"
        self.output = self.path.open("wb")
        try:
            self.child = subprocess.Popen(
                [str(binary), *args], env=env, stdout=self.output, stderr=self.output
            )
        except BaseException:
            self.output.close()
            raise

    def wait_for(self, marker):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if marker in self.path.read_text(errors="replace"):
                return
            require(self.child.poll() is None, "Fixture process exited before readiness")
            time.sleep(0.05)
        raise RehearsalFailure("Fixture readiness deadline exceeded")

    def close(self):
        if self.child.poll() is None:
            self.child.terminate()
            try:
                self.child.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait(timeout=5)
        self.output.close()


def check_http(port):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=15)
    try:
        connection.request("GET", "/upgrade-check")
        response = connection.getresponse()
        require(response.status == 200, f"HTTP forwarding returned status {response.status}")
        require(response.read(1024) == b"locho-upgrade-http", "HTTP forwarding body mismatch")
    finally:
        connection.close()


def check_tcp(port, rejected=False):
    with socket.create_connection(("127.0.0.1", port), timeout=15) as connection:
        connection.settimeout(15)
        try:
            connection.sendall(b"locho-upgrade-tcp")
            body = bytearray()
            while len(body) < len(b"locho-upgrade-tcp"):
                part = connection.recv(64)
                if not part:
                    break
                body.extend(part)
        except ConnectionResetError:
            require(rejected, "TCP forwarding reset unexpectedly")
            return
        if rejected:
            require(not body, "Stale capability unexpectedly forwarded bytes")
        else:
            require(body == b"locho-upgrade-tcp", "TCP forwarding body mismatch")


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--previous-binary", type=Path, required=True)
    parser.add_argument("--previous-version", default="1.2.0",
                        help="Exact published baseline version (default: 1.2.0)")
    parser.add_argument("--candidate-binary", type=Path, required=True)
    return parser.parse_args(argv)


def check_version(output, version):
    require(re.fullmatch(rf"locho {re.escape(version)} \(commit [0-9a-f]{{40}}, clean\)", output),
            "Binary version or clean release provenance mismatch")


def main():
    args = parse_args()
    previous, candidate = args.previous_binary.resolve(), args.candidate_binary.resolve()
    with (Path(__file__).resolve().parent.parent / "Cargo.toml").open("rb") as manifest:
        candidate_version = tomllib.load(manifest)["package"]["version"]
    for binary, version in [(previous, args.previous_version), (candidate, candidate_version)]:
        output = run([str(binary), "--version"])
        check_version(output, version)
        print(f"Verified clean Locho {version} binary")

    with tempfile.TemporaryDirectory(prefix="locho-upgrade-") as temporary, ExitStack() as stack:
        directory = Path(temporary)
        state = directory / "state"
        env = {**os.environ, "LOCHO_STATE_DIR": str(state), "NO_COLOR": "1", "RUST_LOG": "info"}
        certificate, key = directory / "ca.pem", directory / "tls.key"
        run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
             "-keyout", str(key), "-out", str(certificate), "-subj", "/CN=127.0.0.1",
             "-addext", "subjectAltName=IP:127.0.0.1",
             "-addext", "basicConstraints=critical,CA:FALSE",
             "-addext", "extendedKeyUsage=serverAuth",
             "-addext", "keyUsage=critical,digitalSignature,keyEncipherment"])
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(certificate, key)
        http = ThreadingHTTPServer(("127.0.0.1", 0), HttpFixture)
        http.socket = tls.wrap_socket(http.socket, server_side=True)
        http_port = stack.enter_context(serving(http))
        tcp_port = stack.enter_context(serving(EchoServer(("127.0.0.1", 0), EchoFixture)))
        config = directory / "locho.toml"
        config.write_text(
            '[[services]]\nname = "web"\ntype = "http"\n'
            f'upstream = "https://127.0.0.1:{http_port}"\nca_cert = {json.dumps(str(certificate))}\n'
            '\n[[services]]\nname = "database"\ntype = "tcp"\n'
            f'endpoint = "127.0.0.1:{tcp_port}"\n'
        )
        direct = f"127.0.0.1:{free_port()}"
        processes = []

        def start(binary, arguments, marker):
            process = Process(binary, arguments, env, directory, len(processes))
            processes.append(process)
            stack.callback(process.close)
            process.wait_for(marker)
            return process

        def host(binary):
            return start(binary, ["host", "--config", str(config), "--bind-address", direct],
                         "- database (")

        def capability(binary, service):
            return run([str(binary), "secret", service, "--config", str(config)], env)

        def attachment(binary, token, host_id):
            port = free_port()
            process = start(binary, ["attach", host_id, token, "--direct-address", direct,
                                    "--listen", f"127.0.0.1:{port}"], "locho attached")
            return process, port

        old_host = host(previous)
        before = json.loads((state / "host_state.json").read_text())
        host_id = before["endpoint_id"]
        host_key = (state / "host.key").read_bytes()
        tokens = {service: capability(previous, service) for service in ["web", "database"]}

        def exercise(client):
            for service, check in [("web", check_http), ("database", check_tcp)]:
                process, port = attachment(client, tokens[service], host_id)
                try:
                    check(port)
                finally:
                    process.close()

        for client in [previous, candidate]:
            client_version = args.previous_version if client == previous else candidate_version
            print(f"Checking {client_version} client against {args.previous_version} host")
            exercise(client)
        old_host.close()
        print(f"Both client versions pass HTTP/TCP against the {args.previous_version} host")

        new_host = host(candidate)
        require(json.loads((state / "host_state.json").read_text()) == before,
                "Host state changed during executable-only upgrade")
        require((state / "host.key").read_bytes() == host_key, "Host private key changed during upgrade")
        for service, token in tokens.items():
            require(capability(candidate, service) == token, "Service capability changed during upgrade")
        for client in [previous, candidate]:
            client_version = args.previous_version if client == previous else candidate_version
            print(f"Checking {client_version} client against {candidate_version} host")
            exercise(client)
        ticket = run([str(candidate), "share", "database", "--config", str(config), "--ticket"], env)
        require(ticket.startswith("locho1:"), "Candidate ticket prefix mismatch")
        document = json.loads(base64.urlsafe_b64decode(ticket[7:] + "=" * (-len(ticket[7:]) % 4)))
        require(document == {"host_id": host_id, "capability": tokens["database"], "peer_relay_urls": []},
                "Candidate ticket did not retain the original identity/capability")
        new_host.close()
        print("Executable-only upgrade preserves state/key/capabilities; both clients and ticket export pass")

        run([str(candidate), "rotate-secret", "database", "--config", str(config)], env)
        replacement = capability(candidate, "database")
        require(replacement != tokens["database"], "Rotation did not replace the selected capability")
        require(capability(candidate, "web") == tokens["web"], "Rotation changed an unrelated capability")
        rotated_host = host(candidate)
        process, port = attachment(candidate, tokens["database"], host_id)
        check_tcp(port, rejected=True)
        process.wait_for("TCP attachment rejected with status 403")
        process.close()
        process, port = attachment(candidate, replacement, host_id)
        check_tcp(port)
        process.close()
        process, port = attachment(candidate, tokens["web"], host_id)
        check_http(port)
        process.close()
        rotated_host.close()
        require((state / "host.key").read_bytes() == host_key, "Rotation changed the host private key")

        secrets = [value.rsplit(":", 1)[1] for value in [*tokens.values(), replacement]] + [ticket]
        for process in processes:
            text = process.path.read_text(errors="replace")
            require(all(secret not in text for secret in secrets), "Fixture process logs exposed credentials")
        print("Rotation rejects the old capability with 403; replacement and unrelated service pass")
        print("Process logs contain no generated service credentials or ticket")
    print("Executable-only upgrade rehearsal passed")


if __name__ == "__main__":
    try:
        main()
    except RehearsalFailure as error:
        raise SystemExit(f"Upgrade rehearsal failed: {error}") from None
    except Exception as error:
        # Exceptions may include command arguments or state; print only the type.
        raise SystemExit(f"Upgrade rehearsal failed ({type(error).__name__}); private diagnostics withheld") from None
