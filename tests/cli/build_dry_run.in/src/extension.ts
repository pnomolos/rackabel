export function activate(): void {
  const u = new URL("https://example.com/clip");
  console.log(u.host);
}
