#!/usr/bin/env bash
#
# A controller that is not `cm controller`.
#
# `examples/hosted` implements `Directory` against a made-up identity service with groups,
# sub-groups, plans and feature flags, and calls `cyberium::controller::run`. Nothing else
# differs: the same `cm init`, the same workers, the same `cm t`.
#
# What this proves, and what compiling does not: a foreign directory actually serves
# pleas. Two callers in the *same* organisation get different answers because their plans
# differ, and neither number comes from any file on the controller. A caller the directory
# has never heard of is refused differently from one who is known and unauthorised. And an
# upload is refused by that deployment's own rule about where rules live.
#
#   SIRJI=/path/to/sirji scripts/hosted.sh
#
set -euo pipefail

LAB=${LAB:-/tmp/cmhosted}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
CM=${CM:-$ROOT/target/debug/cm}
HOSTED=${HOSTED:-$ROOT/target/debug/hosted-controller}
SIRJI=${SIRJI:-sirji}
HERE=$(cd "$(dirname "$0")" && pwd)
PORT=8811

if [ "$CM" = "$ROOT/target/debug/cm" ]; then
  cargo build --quiet --manifest-path "$ROOT/Cargo.toml"
fi

rm -rf "$LAB"; mkdir -p "$LAB"; cd "$LAB"
say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

# The aliases matter: `examples/hosted` looks callers up by them, the way a real
# deployment would look up a user id. `lee` is deliberately absent from that directory.
SIRJI_HOME=$LAB/acme $SIRJI init >/dev/null
for who in dana ci-nightly lee; do
  SIRJI_HOME=$LAB/$who $SIRJI init >/dev/null
  SIRJI_HOME=$LAB/$who $SIRJI daemon >$who-sirji.log 2>&1 &
done
SIRJI_HOME=$LAB/acme $SIRJI daemon >acme.log 2>&1 &
sleep 4
for who in dana ci-nightly lee; do
  INV=$(SIRJI_HOME=$LAB/acme $SIRJI invite $who | tail -1)
  SIRJI_HOME=$LAB/$who $SIRJI accept acme "$INV" >/dev/null
done
for d in cm-c w1 w2 w3 w4; do
  INV=$(SIRJI_HOME=$LAB/acme $SIRJI device invite $d | tail -1)
  CM_HOME=$LAB/$d SIRJI_HOME=$LAB/$d $CM init --parent "$INV" --root "$LAB/$d/root" >/dev/null
done
for who in dana ci-nightly lee; do
  INV=$(SIRJI_HOME=$LAB/$who $SIRJI device invite t | tail -1)
  CM_HOME=$LAB/t-$who SIRJI_HOME=$LAB/t-$who $CM init --parent "$INV" >/dev/null
done

python3 "$HERE/fakemodel.py" $PORT allow ask "$LAB/prompt.log" >/dev/null 2>&1 &
echo $! >"$LAB/model.pid"
trap 'kill "$(cat "$LAB/model.pid")" 2>/dev/null || true' EXIT
sleep 1

# The only difference in the whole scenario: a different binary. No tenants/ directory is
# ever created, because this deployment does not keep callers in folders.
say "start the custom controller — note there is no tenants/ folder at all"
CM_MODEL_KEY=stand-in CM_MODEL_URL=http://127.0.0.1:$PORT \
CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $HOSTED >cm-c.log 2>&1 &
sleep 3
grep -E "callers known from|tenant\(s\)" cm-c.log | sed 's/^/  /'
test ! -d "$LAB/cm-c/root/tenants" && echo "  ok: no tenants/ directory exists"

for w in w1 w2 w3 w4; do
  CM_HOME=$LAB/$w SIRJI_HOME=$LAB/$w $CM worker --can linux --rate 1 >$w.log 2>&1 &
done
for _ in $(seq 40); do [ "$(grep -c arrived cm-c.log || true)" -ge 4 ] && break; sleep 1; done

t() { who=$1; shift; CM_HOME=$LAB/t-$who SIRJI_HOME=$LAB/t-$who $CM test cm-c@acme "$@"; }

say "dana is on the enterprise plan — her ceiling comes from the directory, not a file"
t dana --plea nightly-regression --count 4 --need linux --dry-run 2>&1 | tail -1 | sed 's/^/  /'

say "ci-nightly is on a trial, in the same organisation — 2 is all it can have"
t ci-nightly --plea nightly-regression --count 4 --need linux --dry-run 2>&1 | tail -1 | sed 's/^/  /'

say "lee is not in the directory — refused differently, because the fix is different"
set +e
t lee --count 1 --need linux --dry-run 2>&1 | tail -1 | cut -c1-100 | sed 's/^/  /'
set -e

say "the group hierarchy and the flags arrived as proven facts"
python3 - "$LAB/prompt.log" <<'PYX'
import json, sys
def check(label, ok):
    print(f"    {'ok  ' if ok else 'FAIL'} {label}")

bodies = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
users = [b["messages"][0]["content"] for b in bodies]
sysps = [b["system"][0]["text"] for b in bodies]
first, trial = users[0], users[1]

check("the sub-group is attested",       "sub_group: requestly" in first)
check("so is the user id",               "user_id: u-10441" in first)
check("so is the plan",                  "plan: enterprise" in first)
check("and the feature flags",           "flags: gpu-machines, long-runs" in first)
check("a different caller, other facts", "plan: trial" in trial)
check("proven, not declared",            first.index("ATTESTED") < first.index("DECLARED"))
# The deployment's own numbers, which no file on this machine contains.
check("the enterprise ceiling is used",  "at most 24" in sysps[0])
check("the trial ceiling is used",       "at most 2" in sysps[1])
# And cm still carries none of the meaning.
check("cm claims no meaning for keys",   "cm read none of them" in sysps[0])
PYX

say "uploading is refused by this deployment's own rule about where rules live"
mkdir -p "$LAB/edit"
printf '# mine\n\n```yaml\nstanding_limit: 2\n```\n' >"$LAB/edit/policy.md"
set +e
CM_HOME=$LAB/t-dana SIRJI_HOME=$LAB/t-dana $CM upload-policy cm-c@acme "$LAB/edit" 2>&1 \
  | tail -1 | cut -c1-108 | sed 's/^/  /'
set -e

say "the deployment's post-processor saw every decision"
grep -c "^decision:" cm-c.log | sed 's/^/  decisions recorded: /'
grep -m2 "^decision:" cm-c.log | cut -c1-108 | sed 's/^/  /'

say "and it can decide, not only watch — a freeze belongs to the fleet, not to a tenant"
pkill -f hosted-controller 2>/dev/null || true
sleep 1
FLEET_FROZEN=1 CM_MODEL_KEY=stand-in CM_MODEL_URL=http://127.0.0.1:$PORT \
CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $HOSTED >>cm-c.log 2>&1 &
sleep 3
for w in w1 w2 w3 w4; do
  CM_HOME=$LAB/$w SIRJI_HOME=$LAB/$w $CM worker --can linux --rate 1 >>$w.log 2>&1 &
done
for _ in $(seq 40); do [ "$(grep -c arrived cm-c.log || true)" -ge 8 ] && break; sleep 1; done
t dana --plea nightly-regression --count 4 --need linux --dry-run 2>&1 | tail -1 | sed 's/^/  /'
echo "  (the same plea got 4 before the freeze)"

say "controller log"
grep -E "policy (weighed|refused)|bill |not a tenant" cm-c.log | cut -c1-108 | sed 's/^/  /'

say "done — lab in $LAB"
