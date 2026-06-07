// A tiny, dependency-free extension entry for the build tests. It touches `URL`
// (a global the host VM lacks without the polyfill banner) so a real build exercises
// the banner, and it has no imports so the bundle needs no SDK / node_modules.
export function activate(): void {
  const u = new URL("https://example.com/clip");
  console.log(`Clip Renamer active at ${u.host}`);
}
