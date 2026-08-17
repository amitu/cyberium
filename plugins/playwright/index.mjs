// Config helper for suites that would rather declare their fleet in the Playwright
// config than in package.json.
//
// It deliberately returns *nothing that changes how Playwright runs*. cm does not
// need a reporter, a globalSetup, or a fixture, and a plugin that installed one
// would be claiming an influence over the run that it does not have and should not
// want. Distribution happens outside the process — `cm playwright` starts N
// Playwright runs on N machines, each an ordinary `--shard=i/N` — which is exactly
// why an existing suite needs no changes to be distributed.
//
// What this does give you is one place to write the fleet down, and a check that
// what you wrote makes sense, at config-eval time rather than five minutes into CI.

import { writeFileSync } from "node:fs";
import { join } from "node:path";

/**
 * Declare how this suite should be distributed.
 *
 * @param {object} options
 * @param {string} options.controller  `name@org` of the cm controller
 * @param {string[]} [options.need]    capabilities a machine must have
 * @param {number} [options.shards]    how many machines to ask for
 * @param {string} [options.runner]    how to start Playwright (default `npx playwright test`)
 * @param {string} [options.writeTo]   directory to write `cm.generated.json` into,
 *                                     for `cm playwright` to read. Defaults to cwd.
 * @returns {{}} an empty config fragment, so it can be spread into defineConfig
 */
export function cm(options) {
  const { controller, need = [], shards, runner, writeTo = process.cwd() } = options ?? {};

  if (!controller || !controller.includes("@")) {
    throw new Error(
      `cm(): controller must look like "cm-c@acme", got ${JSON.stringify(controller)}`,
    );
  }
  if (shards !== undefined && (!Number.isInteger(shards) || shards < 1)) {
    throw new Error(`cm(): shards must be a positive integer, got ${shards}`);
  }
  if (!Array.isArray(need) || need.some((c) => typeof c !== "string")) {
    throw new Error(`cm(): need must be a list of capability strings`);
  }

  // Written rather than returned, because `cm playwright` runs *outside* the
  // Playwright process and so can never see a value we return from here. A TS
  // config is not something a Rust binary can evaluate, and pretending otherwise
  // would mean shipping a TypeScript interpreter to read four fields.
  writeFileSync(
    join(writeTo, "cm.generated.json"),
    JSON.stringify({ controller, need, shards, runner }, null, 2) + "\n",
  );

  return {};
}

export default cm;
