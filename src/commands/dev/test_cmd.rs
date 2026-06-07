//! `rackabel dev test` — run the project's tests / headless smoke (DESIGN §2, §3.8).
//!
//! OWNED BY THE DEV-TEST AGENT. STUB: non-interactive by default (never prompts);
//! `--bail` fails fast; `--` forwards to the runner. No Live, no Developer Mode, no GUI.

use crate::cli::DevTestArgs;
use crate::context::Ctx;
use crate::dev::todo_err;
use crate::error::{CmdResult, ErrorCode};

pub fn run(_args: &DevTestArgs, _ctx: &Ctx) -> CmdResult<()> {
    // BuildRuntime class: a headless test run is a build/runtime activity (exit 1 on
    // failure), per the §7 exit-code taxonomy the dev-test agent inherits.
    todo_err(ErrorCode::ReloadActivateFailed, "`rackabel dev test`")
}
