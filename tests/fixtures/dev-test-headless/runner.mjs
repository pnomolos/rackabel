// A dependency-free headless runner stub (lidal's start:headless/runner.mjs shape).
// Exits 0 (pass) or 1 (fail) per RK_TEST_OUTCOME so the `dev test` (§3.8) integration
// tests can drive both outcomes without a network install.
const outcome = process.env.RK_TEST_OUTCOME || "pass";
process.stdout.write("headless runner: started\n");
if (outcome === "fail") {
  process.stdout.write("headless runner: 1 failed\n");
  process.exit(1);
}
process.stdout.write("headless runner: ok\n");
process.exit(0);
