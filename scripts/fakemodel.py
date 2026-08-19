"""A model that says what it is told to, so the code around it can be tested.

`count` is a number, or `ask` — meaning "behave": give the requester what they asked
for, but never more than the ceiling the prompt stated and never more than the fleet
says is free. That is the model a scenario about transport and allocation wants; the
badly behaved ones are for the scenario about limits.
"""

import http.server, json, re, sys

PORT = int(sys.argv[1])
VERDICT = sys.argv[2]
COUNT = sys.argv[3]
LOG = sys.argv[4]


def behave(system, user):
    """min(asked, stated ceiling, machines free) — read out of the prompt itself."""
    def find(pattern, text, default):
        m = re.search(pattern, text)
        return int(m.group(1)) if m else default

    asked = find(r"count: (\d+)", user, 1)
    ceiling = find(r"at most (\d+)", system, 10 ** 6)
    free = find(r"free right now: (\d+)", user, 10 ** 6)
    return min(asked, ceiling, free)

class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        body = self.rfile.read(n)
        with open(LOG, "ab") as f:
            f.write(body + b"\n")
        sent = json.loads(body)
        if COUNT == "ask":
            count = behave(sent["system"][0]["text"], sent["messages"][0]["content"])
            why = "the fake model gave what was asked, inside the stated limits"
        else:
            count = int(COUNT)
            why = "the fake model decided"
        said = json.dumps({"verdict": VERDICT, "count": count, "rationale": why})
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
