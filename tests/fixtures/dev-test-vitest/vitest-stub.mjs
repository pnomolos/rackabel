// A dependency-free stand-in for `vitest run`, used by the `dev test` (§3.8)
// integration tests so they never need a real vitest network install. It prints a
// vitest-shaped summary line (so rackabel's best-effort count parser has something to
// read) and exits 0 (pass) or 1 (fail) based on RK_TEST_OUTCOME (default: pass).
//
// It also echoes the argv it received as a `RK_ARGS:` line so a test can assert that
// `--` runner args (and the forwarded `--bail=1`) reach the runner verbatim.
const args = process.argv.slice(2);
process.stdout.write("RK_ARGS:" + JSON.stringify(args) + "\n");

const outcome = process.env.RK_TEST_OUTCOME || "pass";
if (outcome === "fail") {
  process.stdout.write(" Test Files  1 failed (1)\n");
  process.stdout.write("      Tests  2 passed | 1 failed\n");
  process.exit(1);
}
process.stdout.write(" Test Files  1 passed (1)\n");
process.stdout.write("      Tests  3 passed\n");
process.exit(0);
