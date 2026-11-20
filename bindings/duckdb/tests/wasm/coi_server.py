# Static server with cross-origin isolation headers (COOP/COEP), needed by the DuckDB-Wasm
# threads bundle (SharedArrayBuffer).  python3 coi_server.py <port> [--gzip]
# --gzip serves the extension builds (build/*/*.duckdb_extension.wasm) gzip-compressed with
# Content-Encoding: gzip, as the published repository does.
import gzip, http.server, os, sys

GZIP = "--gzip" in sys.argv


class H(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cross-Origin-Resource-Policy", "cross-origin")
        super().end_headers()

    def do_GET(self):
        path = self.translate_path(self.path)
        if GZIP and path.endswith(".duckdb_extension.wasm") and os.path.isfile(path):
            body = gzip.compress(open(path, "rb").read(), 9)
            self.send_response(200)
            self.send_header("Content-Type", "application/wasm")
            self.send_header("Content-Encoding", "gzip")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()

    def log_message(self, *a):
        pass


http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
