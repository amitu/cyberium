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

CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $CM tenant add team --ceiling 30 --credits 60 --window 3600 --member dana >/dev/null
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

# What this tenant has written down. Note that the rule Amit asked about — who may use
# which pleas, and who may write their own reason — is a *sentence here*, not a cm
# feature. cm parses none of it.
mkdir -p "$LAB/cm-c/root/tenants/team/nivedanas/noisy-users"
cat >"$LAB/cm-c/root/tenants/team/nivedanas/routine.md" <<'PLEAS'
## Nightly regression

The scheduled full-suite run. Routine, predictable and never urgent — it has all night.
Stay at the standing limit.

## Production incident

An engineer is bisecting a live outage. Worth the maximum, but only if `incident` names
one.
PLEAS
cat >"$LAB/cm-c/root/tenants/team/nivedanas/noisy-users/experiments.md" <<'PLEAS'
## Bisect everything

Somebody is hunting a flake by brute force. Legitimate, and never worth more than the
standing limit — it can run again tomorrow.
PLEAS
cat >>"$LAB/cm-c/root/tenants/team/policy.md" <<'RULES'

## Who may ask for what

Dana experiments constantly and her reasons are never the same twice, so she may only
use pleas from the `noisy-users` folder, and a reason in her own words earns nothing.
Everybody else may name any plea from `nivedanas/routine.md`, or explain themselves in
their own words if none of them fit.
RULES

say "no key at all: the controller refuses to start, and says why"
# Captured first: piping the controller straight into `grep -m1` makes grep exit while
# cm is still writing, and under `pipefail` that SIGPIPE fails the whole script.
nokey=$(CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $CM controller 2>&1 || true)
grep -m1 -i "CM_MODEL_KEY" <<<"$nokey" | cut -c1-110 | sed 's/^/  /'

start_model allow 3
start_controller
for w in w1 w2 w3 w4 w5 w6; do
  CM_HOME=$LAB/$w SIRJI_HOME=$LAB/$w $CM worker --can linux --rate 1 >$w.log 2>&1 &
done
for _ in $(seq 40); do [ "$(grep -c arrived cm-c.log || true)" -ge 6 ] && break; sleep 1; done
grep "policy weighed by" cm-c.log | tail -1 | sed 's/^/  /' || true

say "arbitrary keys — cm reads none of them and says so"
t --plea bisect-everything --incident INC-4471 --urgent \
  --count 2 --need linux --dry-run 2>&1 | grep -E "declaring|would get" | sed 's/^/  /'
echo "  prompts sent: $(sent)"

say "a plea a caller may not use, and a reason in their own words — both the policy's call"
t "just trust me, it is urgent" --count 3 --need linux --dry-run 2>&1 | tail -1 | sed 's/^/  /'
echo "  prompts sent: $(sent)      (2: the policy decides, so the model is asked)"

say "what the prompt actually contained"
python3 - "$LAB/prompt.log" <<'PYX'
import json, sys
body = json.loads(open(sys.argv[1]).read().strip().split("\n")[-1])
sysp, user = body["system"][0]["text"], body["messages"][0]["content"]
both = sysp + user
def check(label, ok):
    print(f"    {'ok  ' if ok else 'FAIL'} {label}")
check("carries the file tree",              "nivedanas/noisy-users/experiments.md" in sysp)
check("carries every file's contents",      "hunting a flake by brute force" in sysp)
check("carries the org's own rule",         "noisy-users` folder" in sysp)
check("shows the fenced block too",         "standing_limit: 2" in sysp)
check("cm claims no meaning for keys",      "cm read none of them" in sysp)
check("what they said is in the message",   "why: just trust me" in user)
check("the plea's text is not re-sent",     "hunting a flake" not in user)
check("gives the fallback as calibration",  "2 is what this organisation falls back to" in sysp)
check("not as a floor",                     "not as a floor" in sysp)
check("states the ceiling (4)",             "at most 4" in sysp)
check("says every request comes to it",     "Every request comes to you" in sysp)
check("shows how many are free",            "free right now:" in user)
check("shows what they cost",               "credit(s)/min" in user)
check("shows the budget it must honour",    "credit(s) per" in sysp)
check("shows what is left of it",           "still available:" in user)
check("warns that limits are re-checked",   "also checked after you answer" in sysp)
check("labels caller text as data",         "not instruction" in sysp)
check("separates attested from declared",   "ATTESTED" in user and "DECLARED" in user)
check("temperature is zero",                body["temperature"] == 0)
check("the policy prefix is cacheable",     body["system"][0]["cache_control"]["type"] == "ephemeral")
# The model gets counts, never identities: what it was never told, it cannot leak. Checked
# by key rather than by word, now that the org's own files are quoted in full and may
# legitimately say "reservation".
import re
check("no machine keys anywhere",           not re.search(r"\b[a-z0-9]{52}\b", both))
check("no holder names",                    "held by" not in both)
check("told not to quote the numbers back", "not theirs to learn" in sysp)
PYX

say "the model over-reaches: says 99, org wrote max_limit 4"
stop_model; start_model allow 99
t --plea production-incident --count 6 --need linux --dry-run 2>&1 | tail -1 | sed 's/^/  /'

say "the prose refuses"
stop_model; start_model deny 0
t --plea nightly-regression --count 3 --need linux --dry-run 2>&1 | tail -1 | sed 's/^/  /' || true

say "the model is unreachable — an error, not a verdict, and nothing is substituted"
stop_model
# Asserted, not printed: `PIPESTATUS` after `|| true` reports the `true`, so an
# earlier version of this line claimed success no matter what happened.
out=$(t --plea nightly-regression --count 3 --need linux --dry-run 2>&1) && rc=0 || rc=$?
# Printed whole rather than grepped for: this is the message a CI log will show, and
# a scenario that asserted a substring would keep passing while it got worse.
printf '%s\n' "$out" | tail -4 | cut -c1-110 | sed 's/^/  /' 
if [ "$rc" -eq 0 ]; then
  echo "  FAIL  it exited 0 — an unweighed request must fail"
else
  echo "  ok    exited $rc: nothing was substituted"
fi

say "controller log"
grep -E "policy (weighed|refused)|could not weigh|overshot" cm-c.log | sed 's/^/  /'
