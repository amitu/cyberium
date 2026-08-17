#!/usr/bin/env node
//
// `cm-playwright` — run this repo's Playwright suite across a cm fleet.
//
// It is a thin shim over `cm playwright`, and thin on purpose. Everything that
// needs judgement — pleading, capability matching, dispatch, bringing the blob
// reports home — happens in cm, which already speaks sirji and already holds the
// device identity. Reimplementing any of that in Node would mean two versions of
// it to keep honest.
//
// What the shim adds is the thing a Node package is actually better at: being
// installable next to the suite, so `npx cm-playwright` works with no separate
// download step and no PATH surgery.

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";
import process from "node:process";

/**
 * Find the cm binary.
 *
 * Explicit env first, then the usual places. We do NOT fall back to something
 * that merely looks plausible: running the wrong binary against a real fleet is
 * worse than saying we could not find the right one.
 */
function findCm() {
  if (process.env.CM_BIN) return process.env.CM_BIN;

  const candidates = [
    join(process.cwd(), "node_modules", ".bin", "cm"),
    join(process.env.HOME ?? "", ".cargo", "bin", "cm"),
    "/usr/local/bin/cm",
    "/opt/homebrew/bin/cm",
  ];
  for (const path of candidates) {
    if (existsSync(path)) return path;
  }
  // Let PATH have the last word; if it is not there either, spawn reports it.
  return "cm";
}

const cm = findCm();
const args = ["playwright", ...process.argv.slice(2)];

const child = spawn(cm, args, { stdio: "inherit" });

child.on("error", (e) => {
  if (e.code === "ENOENT") {
    console.error(
      `cm-playwright: cannot find the cm binary (tried ${cm}).\n` +
        `Set CM_BIN, or build it: https://github.com/amitu/cyberium`,
    );
    process.exit(127);
  }
  console.error(`cm-playwright: ${e.message}`);
  process.exit(1);
});

// Pass the run's verdict straight through. A wrapper that turns a failing suite
// into a passing exit code is the single worst thing a test tool can do.
child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 1);
});
