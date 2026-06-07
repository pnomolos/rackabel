//! `rackabel doctor` — diagnose the environment.
//!
//! OWNED BY THE DOCTOR AGENT. A flutter/expo-style checklist with the `[✓]`/`[!]`/
//! `[✗]` vocabulary, per-failure `help:` lines, and a tail summary. The foundation
//! provides a compiling stub; the doctor-owner builds the full checklist on the
//! `services::{live,node,user_library,toolkit}` APIs.

use crate::cli::DoctorArgs;
use crate::context::Ctx;
use crate::error::CmdResult;

pub fn run(args: &DoctorArgs, _ctx: &Ctx) -> CmdResult<()> {
    let _ = args;
    Err(crate::services::esbuild::not_implemented("doctor"))
}
