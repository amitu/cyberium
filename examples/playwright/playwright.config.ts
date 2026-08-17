import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  // Shard by test rather than by file. With one spec file, file-level sharding
  // would put everything on one machine and quietly make the fleet pointless.
  fullyParallel: true,
  // One worker per machine keeps the demo honest: what you are watching is the
  // fleet doing the parallelism, not one laptop's cores.
  workers: 1,
  reporter: [["list"]],
});
