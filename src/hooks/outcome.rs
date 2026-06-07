//! Per-hook outcome types (DESIGN §5.3 — the stdout/exit-code half of the contract).
//!
//! FOUNDATION-OWNED, FROZEN. After the engine runs a hook subprocess (writes the §5.3
//! stdin object, closes stdin, reads stdout, waits under the timeout), it maps the
//! `(stdout, exit_code, timed_out)` triple to a [`HookOutcome`] per that hook's row in
//! the §5.3 table. This module freezes the OUTCOME shapes; the engine body produces them.
//!
//! The four `doctor_check` combinations (§5.3) live here as [`DoctorLine::resolve`] so the
//! stdout-line-wins precedence is in one tested place:
//!   (a) exit 0 + line ⇒ line wins;
//!   (b) nonzero + line ⇒ line wins (a script can show a row then exit nonzero);
//!   (c) exit 0 + no line ⇒ pass;
//!   (d) nonzero + no line ⇒ generic fail — and a TIMEOUT is combination (d) by
//!       definition (it produced no line).

use serde::{Deserialize, Serialize};

/// The symbol on a `doctor_check` row (§5.3). Drives the row's icon/severity in the
/// `doctor` output exactly like a built-in check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DoctorSymbol {
    Ok,
    Warn,
    Fail,
}

/// One authoritative `doctor_check` line (§5.3): the JSON object a `doctor_check` hook
/// prints on stdout — `{symbol, message, help}`. When present and well-formed it WINS
/// regardless of exit code (combinations a + b). `help` is optional (a passing row needs
/// no next action); `symbol` and `message` are required for a well-formed line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorLine {
    pub symbol: DoctorSymbol,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

impl DoctorLine {
    /// Parse a single stdout line as a `doctor_check` contract line. Returns `None` if
    /// the line is absent or does not match the documented shape — "stdout that does not
    /// match the contract is treated as informational log output, not data" (§5.3). Only
    /// a well-formed `{symbol, message, help?}` object parses.
    pub fn parse(stdout: &str) -> Option<Self> {
        // The contract is "one JSON line". Take the first non-blank line and try it; a
        // trailing log line after the JSON does not disqualify the row.
        let line = stdout.lines().map(str::trim).find(|l| !l.is_empty())?;
        serde_json::from_str::<DoctorLine>(line).ok()
    }

    /// Resolve the four §5.3 combinations into a single [`DoctorResolution`] for a
    /// `(stdout, exit_code, timed_out)` triple. The stdout line wins whenever present
    /// and well-formed; the exit code is consulted ONLY when there is no parseable line.
    /// A timeout produced no line by definition ⇒ combination (d), generic fail.
    pub fn resolve(stdout: &str, exit_code: i32, timed_out: bool) -> DoctorResolution {
        if let Some(line) = Self::parse(stdout) {
            // (a) exit 0 + line, and (b) nonzero + line — the line wins either way.
            return DoctorResolution::Line(line);
        }
        if timed_out {
            // (d) timeout = no line by definition.
            return DoctorResolution::GenericFail;
        }
        if exit_code == 0 {
            // (c) exit 0 + no line ⇒ pass.
            DoctorResolution::Pass
        } else {
            // (d) nonzero + no line ⇒ generic fail row.
            DoctorResolution::GenericFail
        }
    }
}

/// The resolved `doctor_check` outcome after applying the a-d precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorResolution {
    /// The hook's authoritative stdout line drives the row (combinations a + b).
    Line(DoctorLine),
    /// exit 0 + no line ⇒ a silent pass (combination c).
    Pass,
    /// nonzero + no line, or a timeout ⇒ a generic `doctor_check <name> failed` row
    /// (combination d).
    GenericFail,
}

/// The outcome of running ONE hook (DESIGN §5.3). The engine returns this; the caller
/// (build/deploy/dev/doctor/new) interprets it per the phase. The variants encode the
/// four distinct stdout/exit-code contracts across the five hook kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum HookOutcome {
    /// An informational hook (`post_build` / `on_reload`) completed; stdout is ignored.
    /// On a nonzero exit or timeout the engine logs + skips and STILL returns this
    /// variant with `failed = true` — these hooks never abort their phase (§5.3). The
    /// caller does nothing with it beyond logging.
    Informational {
        /// Whether the hook exited nonzero or timed out (logged + skipped, not fatal).
        failed: bool,
    },
    /// A `pre_deploy` decision (§5.3): the ONE veto. `Allow` on exit 0; `Veto` on a
    /// nonzero exit OR a timeout — the deploy ABORTS, framed via §6.1 naming the hook.
    Veto(VetoDecision),
    /// A resolved `doctor_check` row (§5.3) after the a-d precedence.
    Doctor(DoctorResolution),
    /// A `new_template` enumerate result (§5.3): the wizard choice the hook printed, or
    /// `None` when it printed nothing / exited nonzero / timed out (the choice is omitted,
    /// logged).
    Template(Option<TemplateChoice>),
}

/// A `pre_deploy` veto decision (§5.3). The only hook allowed to abort its phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VetoDecision {
    /// exit 0 ⇒ the deploy proceeds.
    Allow,
    /// nonzero exit or timeout ⇒ the deploy ABORTS. `timed_out` distinguishes a deliberate
    /// veto from the bounded-DoS timeout path for the §6.1 frame ("… timed out after Ns").
    Veto { timed_out: bool },
}

/// A `new_template` wizard choice (§5.3): the single line a `new_template` hook prints —
/// an absolute path to a template dir, OR a `gh:owner/repo[@ref]` ref. It becomes a CHOICE
/// in the `new` wizard's template list; if picked it renders through ordinary tier-1
/// machinery (§5.5), with no second call to the hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateChoice {
    /// An absolute path to a local template directory.
    Path(std::path::PathBuf),
    /// A `gh:owner/repo[@ref]` remote template ref.
    Ref(String),
}

impl TemplateChoice {
    /// Classify a `new_template` hook's single stdout line (§5.3): a `gh:` prefix ⇒ a
    /// remote ref; otherwise an absolute path. Returns `None` for blank output or a
    /// non-absolute, non-`gh:` line (the choice is omitted, logged). The `@scope/name`
    /// form is accepted as a ref token too (classified, resolution is tier-1's job).
    pub fn parse(stdout: &str) -> Option<Self> {
        let line = stdout.lines().map(str::trim).find(|l| !l.is_empty())?;
        if let Some(rest) = line.strip_prefix("gh:") {
            if rest.is_empty() {
                return None;
            }
            return Some(Self::Ref(line.to_string()));
        }
        if line.starts_with('@') {
            return Some(Self::Ref(line.to_string()));
        }
        let p = std::path::Path::new(line);
        if p.is_absolute() {
            Some(Self::Path(p.to_path_buf()))
        } else {
            // A relative bareword is not a valid choice line (a path must be absolute,
            // §5.3) — treat as informational log output, omit the choice.
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(symbol: DoctorSymbol, msg: &str, help: Option<&str>) -> DoctorLine {
        DoctorLine {
            symbol,
            message: msg.to_string(),
            help: help.map(str::to_string),
        }
    }

    #[test]
    fn doctor_line_parses_well_formed() {
        let l = DoctorLine::parse(
            r#"{"symbol":"warn","message":"notarize creds missing","help":"set NOTARY_KEY"}"#,
        )
        .unwrap();
        assert_eq!(l.symbol, DoctorSymbol::Warn);
        assert_eq!(l.message, "notarize creds missing");
        assert_eq!(l.help.as_deref(), Some("set NOTARY_KEY"));
    }

    #[test]
    fn doctor_line_help_optional() {
        let l = DoctorLine::parse(r#"{"symbol":"ok","message":"all good"}"#).unwrap();
        assert_eq!(l.symbol, DoctorSymbol::Ok);
        assert!(l.help.is_none());
    }

    #[test]
    fn doctor_line_non_contract_stdout_is_none() {
        assert!(DoctorLine::parse("just some log output").is_none());
        assert!(DoctorLine::parse("").is_none());
        // Missing required `message` ⇒ not well-formed.
        assert!(DoctorLine::parse(r#"{"symbol":"ok"}"#).is_none());
        // Unknown symbol ⇒ not well-formed.
        assert!(DoctorLine::parse(r#"{"symbol":"bogus","message":"x"}"#).is_none());
    }

    #[test]
    fn doctor_line_first_nonblank_line_wins() {
        let out = "\n  {\"symbol\":\"fail\",\"message\":\"boom\"}\nsome trailing log\n";
        let l = DoctorLine::parse(out).unwrap();
        assert_eq!(l.symbol, DoctorSymbol::Fail);
        assert_eq!(l.message, "boom");
    }

    // ---- the four §5.3 precedence combinations a-d ----

    #[test]
    fn precedence_a_exit0_plus_line_line_wins() {
        let out = r#"{"symbol":"warn","message":"w"}"#;
        assert_eq!(
            DoctorLine::resolve(out, 0, false),
            DoctorResolution::Line(line(DoctorSymbol::Warn, "w", None))
        );
    }

    #[test]
    fn precedence_b_nonzero_plus_line_line_wins() {
        // A script that emits a valid row then crashes still shows it (§5.3 (b)).
        let out = r#"{"symbol":"fail","message":"f","help":"do x"}"#;
        assert_eq!(
            DoctorLine::resolve(out, 1, false),
            DoctorResolution::Line(line(DoctorSymbol::Fail, "f", Some("do x")))
        );
    }

    #[test]
    fn precedence_c_exit0_no_line_is_pass() {
        assert_eq!(
            DoctorLine::resolve("informational chatter\n", 0, false),
            DoctorResolution::Pass
        );
    }

    #[test]
    fn precedence_d_nonzero_no_line_is_generic_fail() {
        assert_eq!(
            DoctorLine::resolve("oops\n", 3, false),
            DoctorResolution::GenericFail
        );
    }

    #[test]
    fn precedence_d_timeout_is_generic_fail_even_with_zeroish_exit() {
        // A timeout produced no line by definition ⇒ combination (d), regardless of the
        // exit code we synthesize for the killed process.
        assert_eq!(
            DoctorLine::resolve("", 0, true),
            DoctorResolution::GenericFail
        );
        assert_eq!(
            DoctorLine::resolve("partial log", 137, true),
            DoctorResolution::GenericFail
        );
    }

    #[test]
    fn template_choice_classifies_gh_ref() {
        assert_eq!(
            TemplateChoice::parse("gh:acme/starter@v2\n"),
            Some(TemplateChoice::Ref("gh:acme/starter@v2".to_string()))
        );
    }

    #[test]
    fn template_choice_classifies_scope_ref() {
        assert_eq!(
            TemplateChoice::parse("@acme/starter"),
            Some(TemplateChoice::Ref("@acme/starter".to_string()))
        );
    }

    #[test]
    fn template_choice_classifies_absolute_path() {
        assert_eq!(
            TemplateChoice::parse("/opt/templates/house\n"),
            Some(TemplateChoice::Path(std::path::PathBuf::from(
                "/opt/templates/house"
            )))
        );
    }

    #[test]
    fn template_choice_rejects_relative_and_blank() {
        assert!(TemplateChoice::parse("relative/dir").is_none());
        assert!(TemplateChoice::parse("").is_none());
        assert!(TemplateChoice::parse("gh:").is_none());
    }

    #[test]
    fn doctor_symbol_serde_lowercase() {
        assert_eq!(
            serde_json::to_string(&DoctorSymbol::Fail).unwrap(),
            "\"fail\""
        );
    }
}
