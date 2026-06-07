// A no-op `build:headless` step. `dev test` prefers the non-build `start:headless`
// runner over this, so it should never actually run in the headless-fixture tests.
process.exit(0);
