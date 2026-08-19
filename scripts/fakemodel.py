import http.server, json, os, sys, threading

PORT = int(sys.argv[1])
VERDICT = sys.argv[2]
COUNT = int(sys.argv[3])
LOG = sys.argv[4]

class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        body = self.rfile.read(n)
        with open(LOG, "ab") as f:
            f.write(body + b"\n")
        said = json.dumps({"verdict": VERDICT, "count": COUNT,
                           "rationale": "the fake model decided"})
        # Wrapped in prose and a fence on purpose: real models do this, and cm has
        # to cope rather than treat a formatting habit as a refusal.
        out = json.dumps({"content": [{"type": "text",
              "text": "Here you go:\n```json\n" + said + "\n```\n"}]}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)
    def log_message(self, *a):
        pass

http.server.HTTPServer(("127.0.0.1", PORT), H).serve_forever()
