# Static files from ROOT with cross-origin isolation headers, and
# POST /result?browser=NAME saving the body to OUT/result-NAME.json.
# Usage: browser-serve.py PORT ROOT OUT
import http.server
import os
import sys
from urllib.parse import parse_qs, urlparse

port, root, out = int(sys.argv[1]), sys.argv[2], sys.argv[3]


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map, ".wasm": "application/wasm", ".js": "text/javascript"}

    def __init__(self, *a, **k):
        super().__init__(*a, directory=root, **k)

    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_POST(self):
        browser = parse_qs(urlparse(self.path).query).get("browser", ["unknown"])[0]
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        with open(os.path.join(out, f"result-{browser}.json"), "wb") as f:
            f.write(body)
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, *a):
        pass


http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
