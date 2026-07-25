import argparse
import json
import ssl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        size = 1024
        try:
            size = max(0, min(int(self.path.split("size=")[1].split("&")[0]), 32 * 1024 * 1024))
        except (IndexError, ValueError):
            pass
        metadata = json.dumps({"method": self.command, "path": self.path, "size": size}).encode()
        body = (metadata + b"x" * size)[:size]
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("X-Fixture", "locho-stress")
        self.end_headers()
        self.wfile.write(body)

    do_POST = do_GET

    def log_message(self, fmt, *args):
        print("%s - - %s" % (self.address_string(), fmt % args), flush=True)


parser = argparse.ArgumentParser()
parser.add_argument("--cert", required=True)
parser.add_argument("--key", required=True)
args = parser.parse_args()
server = ThreadingHTTPServer(("0.0.0.0", 8443), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(args.cert, args.key)
server.socket = context.wrap_socket(server.socket, server_side=True)
print("upstream HTTPS listening on 8443", flush=True)
server.serve_forever()
