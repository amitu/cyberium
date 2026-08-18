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
//   CM_CONTROLLER   name@org of the controller. Required — without a fleet this
//                   command has nothing to do, and quietly running here instead
//                   would be indistinguishable from having distributed the run.
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
//   CM_DRY_RUN      ask what the fleet would give, run nothing, hold nothing
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

/**
 * The local Playwright config, for the merge to anchor paths to.
 *
 * Blob reports record the **absolute** directory the tests ran in, and
 * `merge-reports` refuses to combine reports that disagree about it. Machines that
 * fetched the repo themselves each have their own checkout path, so they always
 * disagree — every distributed run would fail at the last step.
 *
 * Playwright's answer is `-c`: given a config, it takes that config's rootDir as
 * the truth. Which is what we want anyway — the merged report should name paths in
 * *your* checkout, the one you are about to go and look at.
 */
function mergeConfig() {
  const explicit = playwrightArgs.findIndex((a) => a === "-c" || a === "--config");
  if (explicit !== -1 && playwrightArgs[explicit + 1]) return playwrightArgs[explicit + 1];
  for (const name of [
    "playwright.config.ts",
    "playwright.config.js",
    "playwright.config.mjs",
    "playwright.config.mts",
    "playwright.config.cjs",
  ]) {
    if (existsSync(name)) return name;
  }
  return null;
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

  // A commit nobody else can fetch produces N identical, mystifying clone failures
  // a minute from now, so it is worth flagging early — but only as a warning.
  // `branch -r --contains` reads *local* remote-tracking refs, which are absent in
  // a checkout that pushes by URL and never fetches. Refusing to run on that
  // evidence blocks a perfectly good commit, which is the worse mistake: the
  // machines will say plainly enough if they cannot get it.
  if (!process.env.CM_REF) {
    if (!git("branch", "-r", "--contains", ref)) {
      console.warn(
        `cm-playwright: cannot confirm ${ref.slice(0, 12)} is pushed (no ` +
          `remote-tracking ref contains it). If the machines cannot fetch it, ` +
          `push it or set CM_REF.`,
      );
    }
    if (git("status", "--porcelain")) {
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

  // Ask what the fleet would give and stop. Useful precisely when you do not yet
  // trust the configuration — it exercises the whole chain, credentials included,
  // without taking a machine from anyone to find out.
  if (process.env.CM_DRY_RUN) args.push("--dry-run");

  for (const capability of list(process.env.CM_NEED)) args.push("--need", capability);
  for (const pair of list(process.env.CM_ENV)) {
    if (!pair.includes("=")) {
      console.error(`cm-playwright: CM_ENV wants K=V pairs, got ${JSON.stringify(pair)}`);
      return 1;
    }
    args.push("--env", pair);
  }

  const verdict = await run(findCm(), args);

  // A rehearsal ran nothing, so there is nothing to merge and no suite verdict to
  // report — only whether the asking itself worked.
  if (process.env.CM_DRY_RUN) return verdict;

  const found = flattenBlobs();
  if (found === 0) {
    console.error("cm-playwright: no shard reports came back — nothing to merge");
    // Still a failure even if cm was happy: a run with no report is not a pass.
    return verdict === 0 ? 1 : verdict;
  }

  console.log(`\nmerging ${found} shard report(s)`);
  const config = mergeConfig();
  const merge = spawnSync(
    "npx",
    [
      "playwright",
      "merge-reports",
      ...(config ? ["-c", config] : []),
      "--reporter=list,html",
      join(BLOB_DIR, "all"),
    ],
    { stdio: "inherit" },
  );

  // The suite's verdict wins over the merge's. A merge that succeeds cannot make a
  // failing suite pass, and turning a red run green is the single worst thing a
  // test tool can do.
  if (verdict !== 0) return verdict;
  return merge.status ?? 1;
}

const controller = process.env.CM_CONTROLLER;

// No controller is a mistake, not a mode.
//
// Falling back to a local run was the first design, and it is the wrong one: a run
// that quietly did not distribute looks exactly like one that did. Same output, same
// exit code, nothing to notice — so a CI job that lost its configuration goes on
// passing while every machine in the fleet sits idle, and nobody finds out until
// somebody wonders why the suite takes twenty minutes again.
//
// Running locally is not being taken away. It is spelled `playwright test`, which is
// clearer than this command pretending to be it.
if (!controller) {
  console.error(
    "cm-playwright: CM_CONTROLLER is not set, so there is no fleet to run on.\n" +
      "  Set it (e.g. CM_CONTROLLER=cm-c@acme) to distribute this suite,\n" +
      "  or run `npx playwright test` to run it here.",
  );
  // Distinct from 1: this is nobody's suite failing, it is this command being
  // invoked without what it needs, and CI should be able to tell those apart.
  process.exit(2);
}

try {
  process.exit(await onTheFleet(controller));
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
