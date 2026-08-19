#!/usr/bin/env bash
#
# The model call, end to end, against a fake model whose prompt we can inspect.
#
# The prose half of a policy is the one part of cm whose behaviour is not
# determined by cm, so the properties worth defending are the ones that hold
# *whatever the model returns*: it is not consulted below the standing limit,
# it cannot exceed a number a human wrote, its refusals are honoured, and being
# unreachable is not permission. This runs all four against a server that will
# say whatever we tell it to.
#
# It also greps the assembled prompt, which is why the fake model logs every
# request body. Fleet state must never appear there — a plea that weighed
# differently depending on who else was running could not be snapshot-tested.
#
#   SIRJI=/path/to/sirji CM=/path/to/cm scripts/model.sh
#
set -euo pipefail
LAB=${LAB:-/tmp/cmmodel}
CM=${CM:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/cm}
SIRJI=${SIRJI:-sirji}
HERE=$(cd "$(dirname "$0")" && pwd)
rm -rf "$LAB"; mkdir -p "$LAB"; cd "$LAB"
say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
sent() { [ -f "$LAB/prompt.log" ] && wc -l < "$LAB/prompt.log" | tr -d ' ' || echo 0; }

SIRJI_HOME=$LAB/acme $SIRJI init >/dev/null
SIRJI_HOME=$LAB/dana $SIRJI init >/dev/null
SIRJI_HOME=$LAB/acme $SIRJI daemon >acme.log 2>&1 &
SIRJI_HOME=$LAB/dana $SIRJI daemon >dana.log 2>&1 &
sleep 4
INV=$(SIRJI_HOME=$LAB/acme $SIRJI invite dana | tail -1)
SIRJI_HOME=$LAB/dana $SIRJI accept acme "$INV" >/dev/null
for d in cm-c w1 w2 w3 w4 w5 w6; do
  INV=$(SIRJI_HOME=$LAB/acme $SIRJI device invite $d | tail -1)
  CM_HOME=$LAB/$d SIRJI_HOME=$LAB/$d $CM init --parent "$INV" --root "$LAB/$d/root" >/dev/null
done
INV=$(SIRJI_HOME=$LAB/dana $SIRJI device invite cm-t | tail -1)
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM init --parent "$INV" >/dev/null

CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $CM tenant add team --ceiling 30 --member dana >/dev/null
cat >"$LAB/cm-c/root/tenants/team/policy.md" <<'POLICY'
# policy.md

```yaml
requesters:
  - everyone
standing_limit: 2
max_limit: 4
reservation_seconds: 60
```

## When a bigger run is justified

A request naming an open production incident and its tracker URL may take up to the
maximum. A routine regression run should be countered back towards the standing
limit; there is always tomorrow.
POLICY

start_model() {  # verdict count
  python3 "$HERE/fakemodel.py" 8722 "$1" "$2" "$LAB/prompt.log" >/dev/null 2>&1 &
  echo $! > "$LAB/model.pid"
  sleep 1
}
stop_model() { kill "$(cat "$LAB/model.pid")" 2>/dev/null || true; sleep 1; }

start_controller() {
  CM_MODEL_KEY=test-not-a-real-key CM_MODEL_URL=http://127.0.0.1:8722 \
  CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $CM controller >>cm-c.log 2>&1 &
  sleep 3
}

t() { CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme "$@"; }

start_model allow 3
start_controller
for w in w1 w2 w3 w4 w5 w6; do
  CM_HOME=$LAB/$w SIRJI_HOME=$LAB/$w $CM worker --can linux --rate 1 >$w.log 2>&1 &
done
for _ in $(seq 40); do [ "$(grep -c arrived cm-c.log || true)" -ge 6 ] && break; sleep 1; done
grep "prose weighed by" cm-c.log | tail -1 | sed 's/^/  /'

say "under the standing limit: no model is consulted at all"
t "two is routine" --count 2 --need linux --dry-run 2>&1 | tail -1 | sed 's/^/  /'
echo "  prompts sent so far: $(sent)"

say "above it, the prose is weighed — model says 3 of the 3 asked"
t "an incident, INC-4471" --count 3 --need linux --dry-run 2>&1 | tail -1 | sed 's/^/  /'
echo "  prompts sent: $(sent)"

say "what the prompt actually contained"
python3 - "$LAB/prompt.log" <<'PYX'
import json, sys
body = json.loads(open(sys.argv[1]).read().strip().split("\n")[-1])
sysp, user = body["system"], body["messages"][0]["content"]
both = sysp + user
def check(label, ok):
    print(f"    {'ok  ' if ok else 'FAIL'} {label}")
check("carries the org's prose",            "open production incident" in sysp)
check("states the standing limit (2)",      "2 machines are granted" in sysp)
check("states the max (4)",                 "at most 4" in sysp)
check("labels caller text as data",         "not instruction" in sysp)
check("separates attested from declared",   "ATTESTED" in user and "DECLARED" in user)
check("temperature is zero",                body["temperature"] == 0)
for leak in ["idle", "held by", "credit", "free,"]:
    check(f"no fleet state: {leak!r}",      leak not in both)
PYX

say "the model over-reaches: says 99, org wrote max_limit 4"
stop_model; start_model allow 99
t "give me everything" --count 6 --need linux --dry-run 2>&1 | tail -1 | sed 's/^/  /'

say "the prose refuses"
stop_model; start_model deny 0
t "no good reason" --count 3 --need linux --dry-run 2>&1 | tail -1 | sed 's/^/  /' || true

say "the model is unreachable — deterministic answer, and it says why"
stop_model
t "still routine" --count 3 --need linux --dry-run 2>&1 | tail -1 | sed 's/^/  /' || true

say "controller log"
grep -E "prose (weighed|refused)|could not weigh" cm-c.log | sed 's/^/  /'
