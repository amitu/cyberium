#!/usr/bin/env bash
#
# A whole cm fleet on one machine: an org sirji with a controller and three
# workers of differing capabilities, plus a developer's own sirji running cm test.
#
# Six processes, two organisations, one script. It walks the paths that only exist
# once everything is really running — capability matching against a live roster,
# work dispatched straight to the machines, a grant reclaimed after the caller
# walks away — and prints every log at the end.
#
#   SIRJI=/path/to/sirji CM=/path/to/cm scripts/fleet.sh
#
# LAB has to stay short: it holds unix sockets, and a socket path longer than
# ~104 bytes cannot be bound at all.
set -euo pipefail

LAB=${LAB:-/tmp/cmlab}
SIRJI=${SIRJI:-sirji}
CM=${CM:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/cm}

rm -rf "$LAB"
mkdir -p "$LAB"
cd "$LAB"

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

# --- two sirji identities -------------------------------------------------
say "sirji init"
SIRJI_HOME=$LAB/acme $SIRJI init | sed 's/^/  acme: /'
SIRJI_HOME=$LAB/dana $SIRJI init | sed 's/^/  dana: /'

SIRJI_HOME=$LAB/acme $SIRJI daemon >acme.log 2>&1 &
SIRJI_HOME=$LAB/dana $SIRJI daemon >dana.log 2>&1 &
sleep 2

# --- pair them ------------------------------------------------------------
say "pair dana <-> acme"
INV=$(SIRJI_HOME=$LAB/acme $SIRJI invite dana | tail -1)
SIRJI_HOME=$LAB/dana $SIRJI accept acme "$INV" | sed 's/^/  /'

# --- devices of the org: one controller, three workers --------------------
say "enrol org devices"
for d in cm-c cm-w-1 cm-w-2 cm-w-3 cm-ops; do
  INV=$(SIRJI_HOME=$LAB/acme $SIRJI device invite $d | tail -1)
  CM_HOME=$LAB/$d SIRJI_HOME=$LAB/$d $CM init --parent "$INV" --root "$LAB/$d/root" \
    | sed "s/^/  $d: /"
done

say "enrol dana's cm test device"
INV=$(SIRJI_HOME=$LAB/dana $SIRJI device invite cm-t | tail -1)
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM init --parent "$INV" | sed 's/^/  cm-t: /'

# --- run the fleet --------------------------------------------------------
say "onboard the payments team as a tenant, with dana in it"
# The alias must be what acme's own sirji calls them, because that is what a
# verified ticket carries and what picks their policy. `--ceiling` is acme's to
# set; dana never sees it, and it is what stops dana's own policy.md from being
# dana's own quota.
# The tenant is a *team*, and dana is a member of it. Self-hosted that is the usual
# shape — one policy and one budget for several people — and it is why a tenant's
# name need not be any caller's alias.
CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $CM tenant add payments --ceiling 3 \
  --credits 60 --window 3600 \
  --member dana --note "the demo team" 2>&1 | sed 's/^/  /'

# Short reservations so a timeout is watchable in one run. This half of the
# configuration is dana's own.
cat >"$LAB/cm-c/root/tenants/payments/policy.md" <<'POLICY'
# policy.md

```yaml
requesters:
  - everyone
standing_limit: 10
reservation_seconds: 8
```

## Standing budgets

Anyone may ask for up to the standing limit without justification.
POLICY

CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $CM tenant list 2>&1 | sed 's/^/  /'

say "pair cm-ops as an admin — by key, on the controller itself"
# Being one of acme's devices is not enough: every worker is one of those. An admin
# is on a list the host writes by hand.
OPS_KEY=$(CM_HOME=$LAB/cm-ops SIRJI_HOME=$LAB/cm-ops $CM whoami | awk '{print $2}')
CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $CM admin add ops "$OPS_KEY" \
  --note "the operator laptop" 2>&1 | sed 's/^/  /'
CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $CM admin list 2>&1 | sed 's/^/  /'

say "start controller and workers"
CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $CM controller >cm-c.log 2>&1 &
sleep 2
# cm-w-1 runs the operator's hygiene scripts around every tenancy. They are the
# machine owner's, not the caller's: nothing a caller sends can skip them, and a
# `--post` that failed takes the machine out of the fleet rather than lending out a
# box that may still hold the last tenant's work.
CM_HOME=$LAB/cm-w-1 SIRJI_HOME=$LAB/cm-w-1 $CM worker --can linux --rate 1 \
  --pre 'echo "[hygiene] scrubbing before the next tenant"' \
  --post 'echo "[hygiene] taking back what was left"' >cm-w-1.log 2>&1 &
CM_HOME=$LAB/cm-w-2 SIRJI_HOME=$LAB/cm-w-2 $CM worker --can linux --rate 2 >cm-w-2.log 2>&1 &
# The gpu box costs eight times the cheap one, so `--need linux` must never pick it
# while an ordinary machine is idle.
CM_HOME=$LAB/cm-w-3 SIRJI_HOME=$LAB/cm-w-3 $CM worker --can linux --can gpu --rate 8 >cm-w-3.log 2>&1 &
for _ in $(seq 30); do
  [ "$(grep -c arrived cm-c.log || true)" -ge 3 ] && break
  sleep 1
done

echo "--- controller sees ---"
grep arrived cm-c.log | sed 's/^/  /' || echo "  (nobody arrived)"

# --- the actual asks ------------------------------------------------------
say "is any of this working?"
# Every hop the real asks depend on, checked in order, taking nobody's machine.
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme --ping 2>&1 | sed 's/^/  /'

say "what would we get? — asked twice, taking nothing either time"
for _ in 1 2; do
  CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
    "sizing a run" --count 3 --need linux --dry-run 2>&1 | grep would | sed 's/^/  /'
done

say "the tenant ceiling is acme's, and dana cannot argue with it"
# dana's own policy.md allows 10. acme's tenant.toml says 3. The lower one wins,
# and the caller is told which limit bit them — otherwise they would go and edit
# a policy that was never the constraint.
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "asking for more than acme allows" --count 10 --need linux --dry-run 2>&1 | sed 's/^/  /'

say "ask for 2 linux machines, and really run something on them"
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "flaky suite, bisecting" --count 2 --need linux \
  --cwd "$LAB" \
  --run 'echo "shard {shard} of {shards} on $(hostname -s)"; echo "report {shard}" > out-{shard}.txt' \
  --collect "out-{shard}.txt" --artifacts "$LAB/collected" 2>&1 | sed 's/^/  /'
echo "  --- what came back ---"
find "$LAB/collected" -type f | sed 's/^/    /'
cat "$LAB"/collected/*/*.txt | sed 's/^/    /'

say "ask for a gpu machine"
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "training smoke test" --count 1 --need gpu \
  --run 'echo "training on the gpu box"' 2>&1 | sed 's/^/  /'

say "a machine that has never seen the code fetches it itself"
# The machine gets a checkout of its own, deleted when the reservation ends. It
# holds nothing beforehand — which is the only assumption a real fleet can make.
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "check out and look around" --count 1 --need linux \
  --repo https://github.com/amitu/cyberium --ref main \
  --dir examples/playwright \
  --run 'echo "in $(pwd)"; ls tests/' 2>&1 | sed 's/^/  /'

say "a command that fails, fails the run"
# The exit code has to survive the trip. A distributed runner that reports success
# for a job that failed is worse than one that does not run at all.
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "something broken" --count 1 --need linux --run 'exit 3' 2>&1 | sed 's/^/  /' \
  && echo "  BUG: cm reported success" || echo "  cm exited non-zero, as it should"

say "ask for something nothing can do"
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "risc-v port" --count 1 --need risc-v --run "make" 2>&1 | sed 's/^/  /' || true

say "ask for more than exist"
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "whole suite at once" --count 9 --need gpu --run "pytest" 2>&1 | sed 's/^/  /' || true

say "walk away holding a grant"
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "crashes halfway through" --count 1 --need gpu --run 'echo "started, then died"' --abandon 2>&1 | sed 's/^/  /'
echo "  (the gpu machine is now held by nobody who will come back)"
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "wants the same gpu" --count 1 --need gpu --run 'echo ran' 2>&1 | sed 's/^/  /' || true
echo "  ... waiting for the timeout ..."
sleep 16
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "wants the same gpu, after the timeout" --count 1 --need gpu --run 'echo ran' 2>&1 | sed 's/^/  /'

say "the admin looks inside"
CM_HOME=$LAB/cm-ops SIRJI_HOME=$LAB/cm-ops $CM admin fleet 2>&1 | sed 's/^/  /'
CM_HOME=$LAB/cm-ops SIRJI_HOME=$LAB/cm-ops $CM admin reservations 2>&1 | sed 's/^/  /'

say "what has the payments team spent?"
# Credits, not currency: the fleet prices machines relative to each other and
# leaves exchange rates to whoever bills.
CM_HOME=$LAB/cm-ops SIRJI_HOME=$LAB/cm-ops $CM admin spend 2>&1 | sed 's/^/  /'

say "cm-w-1 is one of acme's devices too, and still may not look"
# The reason this class exists. A machine that offers capacity has no business
# reading the roster, every live reservation, or anybody's budget.
CM_HOME=$LAB/cm-w-1 SIRJI_HOME=$LAB/cm-w-1 $CM admin fleet 2>&1 | sed 's/^/  /' || true

say "dana is a peer — refused for a different reason, and told so"
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM admin fleet --controller cm-c@acme 2>&1 \
  | sed 's/^/  /' || true

say "the operator's hygiene scripts, around every tenancy on cm-w-1"
grep hygiene cm-w-1.log | sed 's/^/  /' || echo "  (none ran)"

say "worker logs"
for w in cm-w-1 cm-w-2 cm-w-3; do
  echo "  --- $w ---"
  sed 's/^/    /' $w.log
done

say "controller log"
sed 's/^/  /' cm-c.log

say "done — logs in $LAB"
