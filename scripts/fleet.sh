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
for d in cm-c cm-w-1 cm-w-2 cm-w-3; do
  INV=$(SIRJI_HOME=$LAB/acme $SIRJI device invite $d | tail -1)
  CM_HOME=$LAB/$d SIRJI_HOME=$LAB/$d $CM init --parent "$INV" --root "$LAB/$d/root" \
    | sed "s/^/  $d: /"
done

say "enrol dana's cm test device"
INV=$(SIRJI_HOME=$LAB/dana $SIRJI device invite cm-t | tail -1)
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM init --parent "$INV" | sed 's/^/  cm-t: /'

# --- run the fleet --------------------------------------------------------
say "short reservations, so a timeout is watchable in one run"
mkdir -p "$LAB/cm-c/root"
cat >"$LAB/cm-c/root/policy.md" <<'POLICY'
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

say "start controller and workers"
CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $CM controller >cm-c.log 2>&1 &
sleep 2
CM_HOME=$LAB/cm-w-1 SIRJI_HOME=$LAB/cm-w-1 $CM worker --slots 1 --can linux >cm-w-1.log 2>&1 &
CM_HOME=$LAB/cm-w-2 SIRJI_HOME=$LAB/cm-w-2 $CM worker --slots 1 --can linux >cm-w-2.log 2>&1 &
CM_HOME=$LAB/cm-w-3 SIRJI_HOME=$LAB/cm-w-3 $CM worker --slots 2 --can linux --can gpu >cm-w-3.log 2>&1 &
for _ in $(seq 30); do
  [ "$(grep -c arrived cm-c.log || true)" -ge 3 ] && break
  sleep 1
done

echo "--- controller sees ---"
grep arrived cm-c.log | sed 's/^/  /' || echo "  (nobody arrived)"

# --- the actual asks ------------------------------------------------------
say "ask for 2 linux machines"
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "flaky suite, bisecting" --count 2 --need linux --run "pytest -x" 2>&1 | sed 's/^/  /'

say "ask for a gpu machine"
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "training smoke test" --count 1 --need gpu --run "train.py" 2>&1 | sed 's/^/  /'

say "ask for something nothing can do"
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "risc-v port" --count 1 --need risc-v --run "make" 2>&1 | sed 's/^/  /' || true

say "ask for more than exist"
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "whole suite at once" --count 9 --need gpu --run "pytest" 2>&1 | sed 's/^/  /' || true

say "walk away holding a grant"
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "crashes halfway through" --count 1 --need gpu --run "flaky.py" --abandon 2>&1 | sed 's/^/  /'
echo "  (the gpu machine is now held by nobody who will come back)"
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "wants the same gpu" --count 1 --need gpu --run "x" 2>&1 | sed 's/^/  /' || true
echo "  ... waiting for the timeout ..."
sleep 16
CM_HOME=$LAB/cm-t SIRJI_HOME=$LAB/cm-t $CM test cm-c@acme \
  "wants the same gpu, after the timeout" --count 1 --need gpu --run "x" 2>&1 | sed 's/^/  /'

say "worker logs"
for w in cm-w-1 cm-w-2 cm-w-3; do
  echo "  --- $w ---"
  sed 's/^/    /' $w.log
done

say "controller log"
sed 's/^/  /' cm-c.log

say "done — logs in $LAB"
