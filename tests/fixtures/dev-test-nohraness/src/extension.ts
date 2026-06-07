// Dependency-free entry. With no harness, `dev test` runs the best-effort generic
// activate() smoke against the built dist/extension.js. This activate() is safe to call
// with a minimal mock context (it touches nothing real), so the smoke passes.
export function activate(): void {
  console.log("No Harness Fixture active");
}
