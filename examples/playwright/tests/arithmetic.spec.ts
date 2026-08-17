// A suite with nothing cm-specific in it. That is the point: these tests do not
// know they are being distributed, and there is nothing here to change to make it
// happen.
//
// They are pure Node — no browser, no server — because the example is about the
// distribution, and anything needing a browser would make it about the download.
import { test, expect } from "@playwright/test";

// Enough tests that a 3-way split has something to split. Each sleeps a little so
// a distributed run is visibly faster than a serial one rather than theoretically.
for (let n = 1; n <= 12; n++) {
  test(`case ${n} adds up`, async () => {
    await new Promise((r) => setTimeout(r, 400));
    expect(n + n).toBe(2 * n);
  });
}

// The red path deserves a test of its own. A distributed runner that loses a
// failure is worse than no runner: CI goes green on a suite that did not pass.
// Set CM_EXAMPLE_FAIL=1 to make exactly one case fail, wherever it lands.
test("the one that fails when asked to", async () => {
  expect(process.env.CM_EXAMPLE_FAIL ?? "0").toBe("0");
});
