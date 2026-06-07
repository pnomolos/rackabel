// A tiny, dependency-free extension entry so a real build (esbuild) needs no
// node_modules. The `dev test` integration tests build this then run the stub runner.
export function activate(): void {
  const u = new URL("https://example.com/vitest-fixture");
  console.log(`Vitest Fixture active at ${u.host}`);
}
