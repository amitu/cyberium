#!/usr/bin/env node
//
// Stands in for `playwright test` in your `npm test`, and runs the same suite
// across a cm fleet instead of on this machine.
//
// Everything Playwright-shaped lives here rather than in cm: the shard arithmetic,
// the blob reports, the merge. cm's side of it is `cm test`, which knows only how
// to get machines and run a command on them — a runner that understood Playwright
// would owe the same favour to jest, pytest and whatever comes next.
//
// Configured entirely by environment, because `npm test` has nowhere to put flags
// and a CI job has nowhere else to put configuration.
//
//   CM_CONTROLLER   name@org of the controller. Unset ⇒ run locally, as normal.
//   CM_SHARDS       how many machines to ask for (default 4)
//   CM_NEED         capabilities, comma separated: linux,node20
//   CM_ENV          environment for the shards: GRPC_SERVER=off,APP_ENV=staging
//   CM_WHY          the reason the controller weighs (default: this package's name)
//   CM_RUNNER       how to start Playwright (default `npx playwright test`)
//   CM_HOME         which cm identity to use — see cm init
//   CM_BIN          path to the cm binary, if it is somewhere unusual
//
//   CM_REPO         where machines fetch the code (default: this checkout's origin)
//   CM_REF          which commit (default: HEAD, if you have pushed it)
//   CM_DIR          subdirectory to run in (default: where you are, within the repo)
//   CM_SETUP        install step on each machine (default `npm ci`)
//   CM_NO_CLONE     set to use a workspace already on the machine instead
//
// Anything after the script name is passed to Playwright untouched:
//
//   npm test -- --project qa --grep @smoke

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, copyFileSync, readFileSync } from "node:fs";
import { join } from "node:path";
import process from "node:process";

const BLOB_DIR = "cm-blob-report";
// A name we choose, so collecting it afterwards needs no guessing about how
// Playwright would have numbered it.
const BLOB = "blob-report/cm-shard-{shard}.zip";

const playwrightArgs = process.argv.slice(2);

/** Split a comma-separated env value, tolerating spaces and trailing commas. */
function list(value) {
  return (value ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

function packageName() {
  try {
    return JSON.parse(readFileSync("package.json", "utf8")).name ?? "a playwright suite";
  } catch {
    return "a playwright suite";
  }
}

/**
 * Find the cm binary. Explicit env first, then the usual places.
 *
 * We do not fall back to something that merely looks plausible: running the wrong
 * binary against a real fleet is worse than saying we could not find the right one.
 */
function findCm() {
  if (process.env.CM_BIN) return process.env.CM_BIN;
  for (const path of [
    join(process.cwd(), "node_modules", ".bin", "cm"),
    join(process.env.HOME ?? "", ".cargo", "bin", "cm"),
    "/usr/local/bin/cm",
    "/opt/homebrew/bin/cm",
  ]) {
    if (existsSync(path)) return path;
  }
  return "cm"; // let PATH have the last word
}

/** Ask git something about this checkout. `null` if it cannot answer. */
function git(...args) {
  const out = spawnSync("git", args, { encoding: "utf8" });
  if (out.status !== 0) return null;
  return out.stdout.trim() || null;
}

/**
 * Work out what the machines should fetch, from the checkout you are standing in.
 *
 * Deriving it beats asking for it: the commit you want tested is almost always the
 * one you are on, and a `CM_REF` that has to be kept up to date by hand is a `CM_REF`
 * that will one day be wrong without anybody noticing.
 *
 * Returns null if the code says to use a workspace already on the machine.
 */
function workspace() {
  if (process.env.CM_NO_CLONE) return null;

  const repo = process.env.CM_REPO ?? git("remote", "get-url", "origin");
  if (!repo) {
    throw new Error(
      "cannot tell where the machines should fetch the code from — " +
        "set CM_REPO, or CM_NO_CLONE if they already have it",
    );
  }

  const ref = process.env.CM_REF ?? git("rev-parse", "HEAD");
  if (!ref) throw new Error("cannot resolve HEAD — set CM_REF");

  // A commit nobody else can fetch produces three identical, mystifying clone
  // failures a minute from now. Say so here instead, while the fix is obvious.
  if (!process.env.CM_REF) {
    const onRemote = git("branch", "-r", "--contains", ref);
    if (!onRemote) {
      throw new Error(
        `HEAD (${ref.slice(0, 12)}) is not on any remote branch, so the machines ` +
          `cannot fetch it. Push it, or set CM_REF to a commit that is.`,
      );
    }
    const dirty = git("status", "--porcelain");
    if (dirty) {
      console.warn(
        `cm-playwright: you have uncommitted changes — the fleet will test ` +
          `${ref.slice(0, 12)}, not what is on your disk.`,
      );
    }
  }

  // Where we are, relative to the repository root: the suite may live well below it.
  const dir = process.env.CM_DIR ?? git("rev-parse", "--show-prefix") ?? undefined;

  return { repo, ref, dir, setup: process.env.CM_SETUP ?? "npm ci" };
}

/** Run something, inheriting stdio, and resolve with its exit code. */
function run(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: "inherit" });
    child.on("error", reject);
    child.on("exit", (code, signal) => resolve(signal ? 1 : (code ?? 1)));
  });
}

/**
 * No controller configured means no fleet, so run the suite the ordinary way.
 *
 * This is the property that makes the plugin safe to commit: `npm test` keeps
 * working on a laptop with no cm, in a CI job that has not been given a fleet yet,
 * and for the contributor who has never heard of any of this.
 */
async function locally() {
  const [command, ...rest] = (process.env.CM_RUNNER ?? "npx playwright test").split(/\s+/);
  return run(command, [...rest, ...playwrightArgs]);
}

/**
 * Gather the per-shard directories cm wrote into one directory, which is the shape
 * `merge-reports` wants. The shard number is already in each filename.
 */
function flattenBlobs() {
  if (!existsSync(BLOB_DIR)) return 0;
  const flat = join(BLOB_DIR, "all");
  mkdirSync(flat, { recursive: true });

  let found = 0;
  for (const entry of readdirSync(BLOB_DIR, { withFileTypes: true })) {
    if (!entry.isDirectory() || entry.name === "all") continue;
    const dir = join(BLOB_DIR, entry.name);
    for (const file of readdirSync(dir)) {
      if (file.endsWith(".zip")) {
        copyFileSync(join(dir, file), join(flat, file));
        found += 1;
      }
    }
  }
  return found;
}

async function onTheFleet(controller) {
  const shards = Number(process.env.CM_SHARDS ?? 4);
  if (!Number.isInteger(shards) || shards < 1) {
    console.error(`cm-playwright: CM_SHARDS must be a positive integer, got ${process.env.CM_SHARDS}`);
    return 1;
  }

  const runner = process.env.CM_RUNNER ?? "npx playwright test";
  // `{shard}` and `{shards}` are filled in by cm, once per machine. `{shards}` is
  // the number actually granted, not the number asked for — so a counter-offer of
  // three machines still produces a correct, complete three-way split.
  const command = [runner, ...playwrightArgs, "--shard={shard}/{shards}", "--reporter=blob"]
    .join(" ")
    .trim();

  const args = [
    "test",
    controller,
    process.env.CM_WHY ?? `${packageName()} suite`,
    "--count", String(shards),
    "--run", command,
    // Shards do not inherit this environment: a worker is another machine, and
    // anything the run needs has to be sent to it deliberately.
    "--env", `PLAYWRIGHT_BLOB_OUTPUT_FILE=${BLOB}`,
    "--collect", BLOB,
    "--artifacts", BLOB_DIR,
  ];
  const code = workspace();
  if (code) {
    args.push("--repo", code.repo, "--ref", code.ref);
    if (code.dir) args.push("--dir", code.dir);
    if (code.setup) args.push("--setup", code.setup);
    console.log(`machines will fetch ${code.ref.slice(0, 12)} from ${code.repo}`);
  } else {
    // The caller says the machines already have the code — which is only true when
    // they share this filesystem. Honest for a laptop demo, not for a real fleet.
    args.push("--cwd", process.cwd());
  }

  for (const capability of list(process.env.CM_NEED)) args.push("--need", capability);
  for (const pair of list(process.env.CM_ENV)) {
    if (!pair.includes("=")) {
      console.error(`cm-playwright: CM_ENV wants K=V pairs, got ${JSON.stringify(pair)}`);
      return 1;
    }
    args.push("--env", pair);
  }

  const verdict = await run(findCm(), args);

  const found = flattenBlobs();
  if (found === 0) {
    console.error("cm-playwright: no shard reports came back — nothing to merge");
    // Still a failure even if cm was happy: a run with no report is not a pass.
    return verdict === 0 ? 1 : verdict;
  }

  console.log(`\nmerging ${found} shard report(s)`);
  const merge = spawnSync(
    "npx",
    ["playwright", "merge-reports", "--reporter=list,html", join(BLOB_DIR, "all")],
    { stdio: "inherit" },
  );

  // The suite's verdict wins over the merge's. A merge that succeeds cannot make a
  // failing suite pass, and turning a red run green is the single worst thing a
  // test tool can do.
  if (verdict !== 0) return verdict;
  return merge.status ?? 1;
}

const controller = process.env.CM_CONTROLLER;
try {
  process.exit(controller ? await onTheFleet(controller) : await locally());
} catch (e) {
  if (e?.code === "ENOENT") {
    console.error(
      `cm-playwright: cannot find the cm binary (tried ${findCm()}).\n` +
        `Set CM_BIN, or build it: https://github.com/amitu/cyberium`,
    );
    process.exit(127);
  }
  console.error(`cm-playwright: ${e?.message ?? e}`);
  process.exit(1);
}
