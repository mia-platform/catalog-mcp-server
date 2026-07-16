import json
from http.server import BaseHTTPRequestHandler, HTTPServer

SPEC = {
    "openapi": "3.0.0",
    "info": {"title": "Echo API", "version": "1.0.0"},
    "paths": {"/echo": {"get": {
        "operationId": "echoHeaders", "tags": ["echo"],
        "summary": "Echo request headers",
        "responses": {"200": {"description": "ok",
            "content": {"application/json": {"schema": {"type": "object"}}}}}}}},
}

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _json(self, obj):
        b = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(b)))
        self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        if self.path.rstrip("/") == "/openapi/json": return self._json(SPEC)
        if self.path.startswith("/echo"):
            return self._json({"received_headers": {k.lower(): v for k, v in self.headers.items()}})
        self.send_response(404); self.end_headers()

HTTPServer(("127.0.0.1", 8899), H).serve_forever()
