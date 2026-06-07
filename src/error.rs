//! The user-facing error type and the exit-code scheme (DESIGN §6.1, §7).
//!
//! Every *expected* failure surfaces as an [`RkError`]: a three-part frame
//! ("error: <problem>", "--> <location>", "help: <next action>") plus a stable
//! [`ErrorCode`] that `rackabel explain <code>` can describe in long form. The raw
//! `anyhow` chain (a Node/V8 trace, an IO error) is carried separately and only
//! shown under `--raw`/`--verbose` — never as the primary output.

use std::fmt;

/// Severity class → process exit code (DESIGN §7).
///
/// The numbers are *identifiers*, not a scale: cause-attribution precedence is the
/// listed order in [`worst`] (Environment > Validation > BuildRuntime), so CI can
/// tell "this machine isn't set up" (`3`) from "my code is wrong" (`1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitClass {
    Ok = 0,
    BuildRuntime = 1,
    Usage = 2,
    Environment = 3,
    Validation = 4,
}

/// Stable error identifiers for `rackabel explain`. Format: `RK` + 4 digits,
/// grouped by thousands so the code itself hints at the exit class (DESIGN §4 code
/// table). `RK0xxx`/`RK02xx`/`RK03xx` = environment (exit 3), `RK1xxx` =
/// build/runtime (exit 1), `RK4xxx` = validation (exit 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    // -- environment / project setup (exit 3) --
    /// No `rackabel.toml` found in cwd or parents.
    NoManifest,
    /// Project has both or neither `[extension]`/`[device]`.
    AmbiguousKind,
    /// Manifest parse error (bad TOML / unknown field).
    ManifestParse,
    // -- toolkit (exit 3) --
    /// Extensions toolkit (SDK/CLI tarball) not found.
    ToolkitNotFound,
    /// Toolkit found but SDK/CLI version mismatch vs `[toolchain].sdk`.
    ToolkitVersionMismatch,
    // -- environment runtime (exit 3) --
    /// Multiple User Libraries; cannot pick under `--no-input`.
    UserLibraryAmbiguous,
    /// User Library not resolvable (open Live once).
    UserLibraryNotFound,
    /// No Ableton Live install found (or below 12.4.5).
    NoLiveInstall,
    /// Native dep not compiled (`deploy --fix`).
    NativeDepNotCompiled,
    /// No usable node runtime for build (Live below floor → upgrade Live).
    NoNodeRuntime,
    /// Developer Mode off (navigational gate for the dev loop).
    DeveloperModeOff,
    // -- build / runtime (exit 1) --
    /// esbuild bundle failed.
    BuildFailed,
    /// `tsc --noEmit` typecheck failed.
    TypecheckFailed,
    /// Bundle sanity failed (`node --check` / bundle < 10KB).
    BundleSanity,
    /// Deploy copy failed (permissions; names the folder).
    DeployCopyFailed,
    /// Pack failed (official CLI non-zero, wrapped).
    PackFailed,
    // -- validation (exit 4) --
    /// Manifest incomplete (missing name/author/entry/version/minimumApiVersion).
    ManifestIncomplete,
    /// `minimumApiVersion` > detected host apiVersion.
    ApiVersionTooHigh,
    /// Version not bumped vs last packed version.
    VersionNotBumped,
    /// `--include` not relative/inside extension dir, or not found.
    IncludeInvalid,
    /// Stable-identifier drift (command id removed/renamed).
    IdentifierDrift,
}

impl ErrorCode {
    /// The `RKxxxx` string used in error frames and `explain`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoManifest => "RK0001",
            Self::AmbiguousKind => "RK0002",
            Self::ManifestParse => "RK0003",
            Self::ToolkitNotFound => "RK0201",
            Self::ToolkitVersionMismatch => "RK0202",
            Self::UserLibraryAmbiguous => "RK0301",
            Self::UserLibraryNotFound => "RK0302",
            Self::NoLiveInstall => "RK0303",
            Self::NativeDepNotCompiled => "RK0304",
            Self::NoNodeRuntime => "RK0305",
            Self::DeveloperModeOff => "RK0306",
            Self::BuildFailed => "RK1301",
            Self::TypecheckFailed => "RK1302",
            Self::BundleSanity => "RK1303",
            Self::DeployCopyFailed => "RK1304",
            Self::PackFailed => "RK1305",
            Self::ManifestIncomplete => "RK4001",
            Self::ApiVersionTooHigh => "RK4002",
            Self::VersionNotBumped => "RK4003",
            Self::IncludeInvalid => "RK4004",
            Self::IdentifierDrift => "RK4005",
        }
    }

    /// Parse a code string (case-insensitive) back to an [`ErrorCode`].
    pub fn from_str(s: &str) -> Option<Self> {
        let up = s.to_ascii_uppercase();
        Self::ALL.iter().copied().find(|c| c.as_str() == up)
    }

    /// The exit class this code maps to. Keeps the frame and the exit code in sync.
    pub fn class(self) -> ExitClass {
        match self {
            Self::NoManifest
            | Self::AmbiguousKind
            | Self::ManifestParse
            | Self::ToolkitNotFound
            | Self::ToolkitVersionMismatch
            | Self::UserLibraryAmbiguous
            | Self::UserLibraryNotFound
            | Self::NoLiveInstall
            | Self::NativeDepNotCompiled
            | Self::NoNodeRuntime
            | Self::DeveloperModeOff => ExitClass::Environment,
            Self::BuildFailed
            | Self::TypecheckFailed
            | Self::BundleSanity
            | Self::DeployCopyFailed
            | Self::PackFailed => ExitClass::BuildRuntime,
            Self::ManifestIncomplete
            | Self::ApiVersionTooHigh
            | Self::VersionNotBumped
            | Self::IncludeInvalid
            | Self::IdentifierDrift => ExitClass::Validation,
        }
    }

    /// Every code, in declaration order. Used by `explain` to list valid codes.
    pub const ALL: &'static [ErrorCode] = &[
        Self::NoManifest,
        Self::AmbiguousKind,
        Self::ManifestParse,
        Self::ToolkitNotFound,
        Self::ToolkitVersionMismatch,
        Self::UserLibraryAmbiguous,
        Self::UserLibraryNotFound,
        Self::NoLiveInstall,
        Self::NativeDepNotCompiled,
        Self::NoNodeRuntime,
        Self::DeveloperModeOff,
        Self::BuildFailed,
        Self::TypecheckFailed,
        Self::BundleSanity,
        Self::DeployCopyFailed,
        Self::PackFailed,
        Self::ManifestIncomplete,
        Self::ApiVersionTooHigh,
        Self::VersionNotBumped,
        Self::IncludeInvalid,
        Self::IdentifierDrift,
    ];
}

/// A fully-formed three-part error (DESIGN §6.1). `raw` is shown only under
/// `--raw`/`--verbose`.
#[derive(Debug)]
pub struct RkError {
    pub code: ErrorCode,
    pub class: ExitClass,
    /// The plain-English problem, *without* a leading "error:" (the frame adds it).
    pub problem: String,
    /// The offending value/location, rendered after "  --> ".
    pub location: Option<String>,
    /// The literal next action, rendered after "  help: ".
    pub help: String,
    /// The raw internal chain; never primary output.
    pub raw: Option<anyhow::Error>,
}

impl RkError {
    /// Construct an error. `class` is normally `code.class()`; it is taken
    /// explicitly so a call site can deliberately re-class in a rare case, but
    /// callers should pass `code.class()` unless they have a reason not to.
    pub fn new(
        code: ErrorCode,
        class: ExitClass,
        problem: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        Self {
            code,
            class,
            problem: problem.into(),
            location: None,
            help: help.into(),
            raw: None,
        }
    }

    /// Construct an error whose exit class is derived from the code (the common case).
    pub fn of(code: ErrorCode, problem: impl Into<String>, help: impl Into<String>) -> Self {
        Self::new(code, code.class(), problem, help)
    }

    /// Builder: attach the offending value/location (the "--> " line).
    #[must_use]
    pub fn at(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    /// Builder: attach the raw internal error chain (gated behind `--raw`).
    #[must_use]
    pub fn raw(mut self, e: anyhow::Error) -> Self {
        self.raw = Some(e);
        self
    }
}

impl fmt::Display for RkError {
    /// Plain (non-color) rendering of the frame. The color-aware path is
    /// [`crate::ui::frame::print_error`]; this exists so `RkError` is a real
    /// `std::error::Error` and shows up sanely in logs / `{:?}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error: {} [{}]", self.problem, self.code.as_str())?;
        if let Some(loc) = &self.location {
            write!(f, "\n  --> {loc}")?;
        }
        write!(f, "\n  help: {}", self.help)
    }
}

impl std::error::Error for RkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.raw
            .as_ref()
            .map(|e| e.as_ref() as &dyn std::error::Error)
    }
}

impl From<RkError> for std::process::ExitCode {
    fn from(e: RkError) -> Self {
        std::process::ExitCode::from(e.class as u8)
    }
}

pub type CmdResult<T> = std::result::Result<T, RkError>;

/// Combine many gate results into the single highest-severity exit class.
///
/// Precedence (DESIGN §7 cause attribution): Environment(3) > Validation(4) >
/// BuildRuntime(1). Environment short-circuits first. Returns `None` if `errors`
/// is empty. Note this is *cause-attribution* precedence, not numeric ordering —
/// commands that run gates in sequence should run the environment subset first and
/// return immediately, but `worst` exists for the rare aggregate case.
pub fn worst(errors: &[RkError]) -> Option<ExitClass> {
    if errors.is_empty() {
        return None;
    }
    let has = |c: ExitClass| errors.iter().any(|e| e.class == c);
    if has(ExitClass::Environment) {
        Some(ExitClass::Environment)
    } else if has(ExitClass::Validation) {
        Some(ExitClass::Validation)
    } else if has(ExitClass::BuildRuntime) {
        Some(ExitClass::BuildRuntime)
    } else {
        // Usage / Ok would be unusual in this aggregate, fall back to first.
        Some(errors[0].class)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_roundtrips() {
        for &code in ErrorCode::ALL {
            assert_eq!(ErrorCode::from_str(code.as_str()), Some(code));
            // case-insensitive
            assert_eq!(
                ErrorCode::from_str(&code.as_str().to_lowercase()),
                Some(code)
            );
        }
        assert_eq!(ErrorCode::from_str("RK9999"), None);
        assert_eq!(ErrorCode::from_str("nonsense"), None);
    }

    #[test]
    fn code_class_matches_thousands_grouping() {
        assert_eq!(ErrorCode::NoManifest.class(), ExitClass::Environment);
        assert_eq!(ErrorCode::ToolkitNotFound.class(), ExitClass::Environment);
        assert_eq!(ErrorCode::BuildFailed.class(), ExitClass::BuildRuntime);
        assert_eq!(ErrorCode::ManifestIncomplete.class(), ExitClass::Validation);
    }

    #[test]
    fn exit_code_is_class_number() {
        let e = RkError::of(ErrorCode::BuildFailed, "x", "y");
        let code: std::process::ExitCode = e.into();
        // ExitCode has no Eq; compare via debug.
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", std::process::ExitCode::from(1))
        );
    }

    #[test]
    fn worst_enforces_precedence() {
        let env = RkError::of(ErrorCode::NoLiveInstall, "a", "b");
        let val = RkError::of(ErrorCode::ManifestIncomplete, "a", "b");
        let build = RkError::of(ErrorCode::BuildFailed, "a", "b");
        assert_eq!(worst(&[val, build]), Some(ExitClass::Validation));
        let env2 = RkError::of(ErrorCode::NoLiveInstall, "a", "b");
        let build2 = RkError::of(ErrorCode::BuildFailed, "a", "b");
        assert_eq!(worst(&[env2, build2]), Some(ExitClass::Environment));
        // env beats validation
        let val2 = RkError::of(ErrorCode::ManifestIncomplete, "a", "b");
        assert_eq!(worst(&[val2, env]), Some(ExitClass::Environment));
        assert_eq!(worst(&[]), None);
    }

    #[test]
    fn display_renders_three_parts() {
        let e = RkError::of(
            ErrorCode::NoManifest,
            "no manifest found",
            "run `rackabel new`",
        )
        .at("looked in /tmp/x");
        let s = e.to_string();
        assert!(s.contains("error: no manifest found"));
        assert!(s.contains("--> looked in /tmp/x"));
        assert!(s.contains("help: run `rackabel new`"));
        assert!(s.contains("RK0001"));
    }
}
