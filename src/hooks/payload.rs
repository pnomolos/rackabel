//! Per-hook stdin payload structs (DESIGN §5.3 — the per-hook I/O contract TABLE).
//!
//! FOUNDATION-OWNED, FROZEN. Each struct serializes EXACTLY the §5.3 field names/types
//! for one [`super::HookKind`]; the engine writes exactly one of these as a single JSON
//! object to the hook's stdin and then CLOSES stdin (EOF framing, §5.3).
//!
//! ## The two contract rules these structs encode
//!
//! 1. **Field types** (§5.3): `project_dir`, `bundle_path`, `user_library` are absolute
//!    path STRINGS; `slug`, `name`, `kind`, `build_hash` are STRINGS; `release`, `ok` are
//!    BOOLEANS; `reload_ms` is a NUMBER. `manifest_toml` is the project's `rackabel.toml`
//!    PARSED and rendered as a JSON OBJECT (NOT a path) — see [`manifest_toml_object`].
//!
//! 2. **Unset/optional = OMITTED, never empty** (§5.3, mirroring §5.2's
//!    commit-unset-not-empty): a field with no value in a context is dropped from the
//!    object entirely (`skip_serializing_if`), never sent as `""` or as an explicit
//!    `null`. `bundle_path` is absent when a build was skipped; `project_dir` /
//!    `manifest_toml` are absent when `doctor_check` runs outside any project. A hook
//!    TESTS PRESENCE, never an empty string.
//!
//! Paths serialize as strings (we hold them as `String` already so the JSON is exactly
//! the absolute-path string §5.3 specifies, with no platform-dependent `PathBuf`
//! escaping).

use serde::Serialize;

/// `post_build` stdin (§5.3): `{project_dir, manifest_toml, bundle_path?, build_hash,
/// kind, release}`. stdout ignored; nonzero/timeout ⇒ logged + skipped (never aborts).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PostBuildPayload {
    /// Absolute path to the project root.
    pub project_dir: String,
    /// The parsed `rackabel.toml` as a JSON object (see [`manifest_toml_object`]).
    pub manifest_toml: serde_json::Value,
    /// Absolute path to the built bundle. ABSENT when the build was skipped (§5.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_path: Option<String>,
    /// The build hash (a string).
    pub build_hash: String,
    /// The project kind string (e.g. `"extension"`).
    pub kind: String,
    /// Whether this was a release build.
    pub release: bool,
}

/// `pre_deploy` stdin (§5.3): `{project_dir, manifest_toml, bundle_path, user_library,
/// slug}`. stdout ignored; **nonzero/timeout ABORTS the deploy** (the one veto hook).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PreDeployPayload {
    /// Absolute path to the project root.
    pub project_dir: String,
    /// The parsed `rackabel.toml` as a JSON object.
    pub manifest_toml: serde_json::Value,
    /// Absolute path to the built bundle (always present for a deploy).
    pub bundle_path: String,
    /// Absolute path to the resolved User Library.
    pub user_library: String,
    /// The install slug (the project dir basename).
    pub slug: String,
}

/// `on_reload` stdin (§5.3): `{project_dir, manifest_toml, name, reload_ms, ok}`.
/// stdout ignored; nonzero/timeout ⇒ logged + skipped.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OnReloadPayload {
    /// Absolute path to the project root.
    pub project_dir: String,
    /// The parsed `rackabel.toml` as a JSON object.
    pub manifest_toml: serde_json::Value,
    /// The reloaded extension's name (a string).
    pub name: String,
    /// The reload duration in milliseconds (a number).
    pub reload_ms: u64,
    /// Whether the reload succeeded.
    pub ok: bool,
}

/// `doctor_check` stdin (§5.3): `{project_dir?, manifest_toml?}` — **both absent when
/// doctor runs OUTSIDE a project** (an environment command, §5.2/§6.2). The hook MUST
/// tolerate a no-project payload. One JSON line on stdout is authoritative (precedence
/// a-d, see [`super::outcome::DoctorLine`]).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DoctorCheckPayload {
    /// Absolute path to the project root. ABSENT outside a project (§5.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    /// The parsed `rackabel.toml` as a JSON object. ABSENT outside a project (§5.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_toml: Option<serde_json::Value>,
}

/// `new_template` stdin (§5.3): `{kind}` ONLY — no `wizard_answers`, no `project_dir`
/// (neither exists yet; it runs PRE-wizard, before any project is scaffolded). One line
/// on stdout (an absolute template-dir path OR a `gh:owner/repo[@ref]` ref) adds a CHOICE.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NewTemplatePayload {
    /// The kind the wizard is scaffolding (e.g. `"extension"`).
    pub kind: String,
}

/// A typed envelope over the five payloads so the frozen [`super::engine::run_hook`]
/// signature takes ONE `payload` argument. Each variant carries the §5.3 struct for its
/// kind; [`HookPayload::kind`] returns the matching [`super::HookKind`], and
/// [`HookPayload::to_json`] renders the exact stdin object the engine writes.
#[derive(Debug, Clone, PartialEq)]
pub enum HookPayload {
    PostBuild(PostBuildPayload),
    PreDeploy(PreDeployPayload),
    OnReload(OnReloadPayload),
    DoctorCheck(DoctorCheckPayload),
    NewTemplate(NewTemplatePayload),
}

impl HookPayload {
    /// The hook kind this payload is for (so the engine can assert kind/payload agree).
    pub fn kind(&self) -> super::HookKind {
        match self {
            Self::PostBuild(_) => super::HookKind::PostBuild,
            Self::PreDeploy(_) => super::HookKind::PreDeploy,
            Self::OnReload(_) => super::HookKind::OnReload,
            Self::DoctorCheck(_) => super::HookKind::DoctorCheck,
            Self::NewTemplate(_) => super::HookKind::NewTemplate,
        }
    }

    /// Render the exact stdin JSON object the engine writes (then closes stdin). The
    /// omitted-not-empty rule is enforced by the per-struct `skip_serializing_if`.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::PostBuild(p) => serde_json::to_value(p),
            Self::PreDeploy(p) => serde_json::to_value(p),
            Self::OnReload(p) => serde_json::to_value(p),
            Self::DoctorCheck(p) => serde_json::to_value(p),
            Self::NewTemplate(p) => serde_json::to_value(p),
        }
        .expect("hook payloads serialize infallibly (no maps with non-string keys)")
    }
}

/// Convert a parsed `rackabel.toml` (its TEXT) into the `manifest_toml` JSON object
/// (§5.3): the project's manifest "parsed and rendered as a JSON object (not a path)",
/// whose keys mirror §4.2 (`extension`, `host`, `toolchain`, `meta`, …). A hook author
/// parses NOTHING — they get the object directly.
///
/// We parse to a generic [`toml::Value`] (NOT the strict [`crate::manifest::ManifestRaw`]
/// with `deny_unknown_fields`) so a forward manifest carrying tables a hook cares about —
/// including the project-local `[hooks]` table itself — survives into the object verbatim,
/// exactly as on disk. `toml::Value` → `serde_json::Value` is a lossless structural
/// re-render (TOML tables → JSON objects, arrays → arrays, datetimes → their string form).
///
/// Returns the framed parse error already used for `rackabel.toml` (RK0003) if the text is
/// not valid TOML; the caller building a payload should already have a parseable manifest.
pub fn manifest_toml_object(toml_text: &str) -> crate::error::CmdResult<serde_json::Value> {
    let value: toml::Value = toml::from_str(toml_text).map_err(|e| {
        crate::error::RkError::of(
            crate::error::ErrorCode::ManifestParse,
            "rackabel.toml could not be parsed for the hook payload",
            "fix the TOML syntax shown above and rerun",
        )
        .raw(e.into())
    })?;
    // `toml::Value` and `serde_json::Value` are both serde data models; round-tripping
    // through serde_json::to_value gives the faithful JSON-object rendering §5.3 wants.
    Ok(serde_json::to_value(value).expect("a toml::Value always re-renders as JSON"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative parsed manifest object reused across the golden tests.
    fn manifest_obj() -> serde_json::Value {
        manifest_toml_object(
            "[extension]\nname = \"harmonic-lens\"\nversion = \"0.2.0\"\n\
             [toolchain]\nsdk = \"1.0.0\"\n",
        )
        .unwrap()
    }

    #[test]
    fn manifest_toml_is_a_json_object_mirroring_4_2() {
        let obj = manifest_obj();
        // It is an OBJECT, not a path string.
        assert!(obj.is_object());
        assert_eq!(obj["extension"]["name"], "harmonic-lens");
        assert_eq!(obj["extension"]["version"], "0.2.0");
        assert_eq!(obj["toolchain"]["sdk"], "1.0.0");
    }

    #[test]
    fn manifest_toml_preserves_forward_tables_like_hooks() {
        // The strict ManifestRaw would reject unknown tables; the payload object keeps
        // them verbatim (a hook may read its own project-local [hooks] table, etc.).
        let obj = manifest_toml_object(
            "[extension]\nname=\"x\"\n[hooks]\npost_build=\".rackabel/hooks/pb\"\n",
        )
        .unwrap();
        assert_eq!(obj["hooks"]["post_build"], ".rackabel/hooks/pb");
    }

    #[test]
    fn manifest_toml_parse_error_is_rk0003() {
        let err = manifest_toml_object("[extension").unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::ManifestParse);
    }

    // ---- GOLDEN JSON per hook kind (the frozen stdin shapes) ----

    #[test]
    fn golden_post_build_full() {
        let p = HookPayload::PostBuild(PostBuildPayload {
            project_dir: "/p/proj".to_string(),
            manifest_toml: manifest_obj(),
            bundle_path: Some("/p/proj/dist/extension.js".to_string()),
            build_hash: "abc123".to_string(),
            kind: "extension".to_string(),
            release: true,
        });
        let v = p.to_json();
        assert_eq!(p.kind(), crate::hooks::HookKind::PostBuild);
        // Exact field names + types.
        assert_eq!(v["project_dir"], "/p/proj");
        assert!(v["manifest_toml"].is_object());
        assert_eq!(v["bundle_path"], "/p/proj/dist/extension.js");
        assert_eq!(v["build_hash"], "abc123");
        assert_eq!(v["kind"], "extension");
        assert_eq!(v["release"], serde_json::json!(true));
        // Exactly the six keys, no more.
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys.len(), 6, "post_build has 6 keys when bundle present");
    }

    #[test]
    fn golden_post_build_omits_bundle_when_skipped() {
        // bundle_path ABSENT (not "", not null) when a build was skipped (§5.3).
        let p = HookPayload::PostBuild(PostBuildPayload {
            project_dir: "/p".to_string(),
            manifest_toml: manifest_obj(),
            bundle_path: None,
            build_hash: "h".to_string(),
            kind: "extension".to_string(),
            release: false,
        });
        let v = p.to_json();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("bundle_path"), "absent, not empty/null");
        assert_eq!(obj.len(), 5);
    }

    #[test]
    fn golden_pre_deploy() {
        let p = HookPayload::PreDeploy(PreDeployPayload {
            project_dir: "/p".to_string(),
            manifest_toml: manifest_obj(),
            bundle_path: "/p/dist/extension.js".to_string(),
            user_library: "/Users/x/Music/Ableton/User Library".to_string(),
            slug: "harmonic-lens".to_string(),
        });
        let v = p.to_json();
        assert_eq!(v["project_dir"], "/p");
        assert_eq!(v["bundle_path"], "/p/dist/extension.js");
        assert_eq!(v["user_library"], "/Users/x/Music/Ableton/User Library");
        assert_eq!(v["slug"], "harmonic-lens");
        assert_eq!(v.as_object().unwrap().len(), 5);
    }

    #[test]
    fn golden_on_reload() {
        let p = HookPayload::OnReload(OnReloadPayload {
            project_dir: "/p".to_string(),
            manifest_toml: manifest_obj(),
            name: "harmonic-lens".to_string(),
            reload_ms: 142,
            ok: true,
        });
        let v = p.to_json();
        assert_eq!(v["name"], "harmonic-lens");
        // reload_ms is a NUMBER, not a string.
        assert_eq!(v["reload_ms"], serde_json::json!(142));
        assert!(v["reload_ms"].is_number());
        assert_eq!(v["ok"], serde_json::json!(true));
        assert_eq!(v.as_object().unwrap().len(), 5);
    }

    #[test]
    fn golden_doctor_check_in_project() {
        let p = HookPayload::DoctorCheck(DoctorCheckPayload {
            project_dir: Some("/p".to_string()),
            manifest_toml: Some(manifest_obj()),
        });
        let v = p.to_json();
        assert_eq!(v["project_dir"], "/p");
        assert!(v["manifest_toml"].is_object());
        assert_eq!(v.as_object().unwrap().len(), 2);
    }

    #[test]
    fn golden_doctor_check_outside_project_is_empty_object() {
        // BOTH fields absent (not null) when doctor runs outside any project (§5.3).
        let p = HookPayload::DoctorCheck(DoctorCheckPayload {
            project_dir: None,
            manifest_toml: None,
        });
        let v = p.to_json();
        assert_eq!(v, serde_json::json!({}), "an empty object, no null keys");
    }

    #[test]
    fn golden_new_template_kind_only() {
        // {kind} ONLY — no wizard_answers, no project_dir (§5.3).
        let p = HookPayload::NewTemplate(NewTemplatePayload {
            kind: "extension".to_string(),
        });
        let v = p.to_json();
        assert_eq!(v, serde_json::json!({ "kind": "extension" }));
        assert_eq!(v.as_object().unwrap().len(), 1);
    }
}
