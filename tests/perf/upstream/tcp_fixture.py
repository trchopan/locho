import socketserver


class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        while True:
            data = self.request.recv(65536)
            if not data:
                return
            self.request.sendall(data)


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


print("upstream TCP listening on 9000", flush=True)
Server(("0.0.0.0", 9000), Handler).serve_forever()
