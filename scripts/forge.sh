#!/usr/bin/env bash
#
# Mints a token, then rewrites one claim in it and keeps the original signature.
#
#   forge.sh <key.pem> <audience>
#
# Valid base64, valid JSON, wrong signature — so the only thing that can reject it is the
# signature check. Truncating a token instead would only prove that base64 decoding works,
# which is not the property worth testing.
set -euo pipefail
bash "$(dirname "$0")/mint.sh" "$1" "$2" repository=acme/payments | python3 -c '
import sys, base64, json
header, payload, signature = sys.stdin.read().strip().split(".")
claims = json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))
claims["repository"] = "evil/takeover"
forged = base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip("=")
print(f"{header}.{forged}.{signature}")'
