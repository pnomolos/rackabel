//! The Extension project scaffolder (DESIGN §2 `new`, §4.7; SPEC A §3).
//!
//! This is rackabel's *forked equivalent* of `@ableton-extensions/create-extension`:
//! it emits the same project shape (`manifest.json` derived form, `package.json` with
//! `file:` SDK/CLI deps, `tsconfig.json`, `src/extension.ts`, `.gitignore`, `.env`)
//! but **post-processed into rackabel form** per §4.7:
//!   - a `rackabel.toml` is the single source of truth (§4.1); `manifest.json` is left
//!     to `rackabel build` to generate (§4.5), so we do NOT write a hand-maintained one;
//!   - `build.ts` is omitted — the rackabel pipeline (with the polyfill banner the
//!     official `build.ts` lacks, §4.6) replaces it; the `package.json` scripts point at
//!     `rackabel` instead of `tsx build.ts` + `extensions-cli`;
//!   - the vendored SDK/CLI tarballs are kept and wired via `file:` deps (SPEC A §3.4);
//!   - `.gitignore` includes `.rackabel/`.
//!
//! When the official `create-extension` tarball is also present in the discovered
//! toolkit, [`super::generate`] drives *its* scaffold non-interactively and then this
//! module's [`postprocess`] converts the emitted output into the above rackabel form;
//! when it is absent, [`render`] produces the same shape directly. Either way the
//! result is identical, so the rest of `new` is agnostic to which path ran.

use std::path::Path;

use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};

/// The fields the templates interpolate (the §3.5 `data` object, rackabel-shaped).
pub struct ScaffoldData {
    /// Sanitized npm-package name (SPEC A §3.4 `sanitizePackageName`). Used for the
    /// package.json `name`, the manifest display name, and the in-code command/menu ids.
    pub package_name: String,
    /// The human display name the wizard captured (echoed in rackabel.toml as a comment
    /// hint; the manifest uses the sanitized form to match the official toolchain).
    pub display_name: String,
    pub author: String,
    pub license: String,
    /// The SDK tarball basename to reference as `file:./vendor/<name>` (SPEC A §3.4).
    pub sdk_dep_basename: String,
    /// The CLI tarball basename to reference as `file:./vendor/<name>`.
    pub cli_dep_basename: String,
    /// The Extensions API version (SDK `EXTENSIONS_API_VERSIONS[0]`, default 1.0.0).
    pub api_version: String,
    /// Bare skeleton (`--minimal`): no working example, fewer files (DESIGN §2).
    pub minimal: bool,
}

/// The conventional source-entry path the template writes.
pub const SRC_ENTRY: &str = "src/extension.ts";

/// SPEC A §3.4: `name.toLowerCase().replace(/[^a-z0-9]+/g,"-").replace(/^-|-$/g,"")`.
/// The same sanitization the official scaffolder applies to the package/manifest name.
pub fn sanitize_package_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut last_dash = false;
    for ch in lower.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Render the forked rackabel-form Extension scaffold into `root` (which must already
/// exist and be empty enough). Writes every file natively — no `create-extension`
/// needed. The SDK/CLI tarballs are vendored separately by the caller (toolkit::vendor_into)
/// before this runs, so the `file:` deps resolve.
pub fn render(root: &Path, data: &ScaffoldData) -> CmdResult<()> {
    write_file(root, ".gitignore", &gitignore())?;
    write_file(root, "rackabel.toml", &rackabel_toml(data))?;
    write_file(root, "package.json", &package_json(data))?;
    write_file(root, "tsconfig.json", &tsconfig_json())?;
    write_file(root, ".env", &env_file())?;
    write_file(root, "README.md", &readme(data))?;
    write_file(root, SRC_ENTRY, &extension_ts(data))?;
    Ok(())
}

/// Post-process the output of the official `create-extension` into rackabel form
/// (DESIGN §4.7). The official scaffolder has already written its files into `root`
/// (incl. the vendored `vendor/*.tgz`); we:
///   1. read the emitted `manifest.json` + `package.json` to derive `rackabel.toml`;
///   2. drop `build.ts` (the rackabel pipeline replaces it, §4.6) and rewrite
///      `package.json` scripts to call `rackabel`;
///   3. ensure `.gitignore` includes `.rackabel/`;
///   4. leave `manifest.json` for `rackabel build` to regenerate (we remove the
///      hand-emitted one so the single-source-of-truth is `rackabel.toml`, §4.5).
///
/// Note: for 0.2 the *forked* [`render`] path is the one exercised (the gated
/// create-extension tarball is not vendored into rackabel and is not present in tests);
/// `postprocess` is wired so the reuse path of §4.7 is honored the moment the official
/// scaffolder is available, without painting it into a corner.
pub fn postprocess(root: &Path, data: &ScaffoldData) -> CmdResult<()> {
    // 1. Derive rackabel.toml from whatever the official scaffold emitted, falling
    //    back to the wizard data (the emitted manifest mirrors `data` anyway).
    let derived = derive_data_from_emitted(root).unwrap_or_else(|| ScaffoldData {
        package_name: data.package_name.clone(),
        display_name: data.display_name.clone(),
        author: data.author.clone(),
        license: data.license.clone(),
        sdk_dep_basename: data.sdk_dep_basename.clone(),
        cli_dep_basename: data.cli_dep_basename.clone(),
        api_version: data.api_version.clone(),
        minimal: data.minimal,
    });
    write_file(root, "rackabel.toml", &rackabel_toml(&derived))?;

    // 2. Drop build.ts; rewrite package.json scripts to the rackabel pipeline.
    let _ = std::fs::remove_file(root.join("build.ts"));
    write_file(root, "package.json", &package_json(&derived))?;

    // 3. Ensure .gitignore covers .rackabel/.
    write_file(root, ".gitignore", &gitignore())?;

    // 4. The hand-emitted manifest.json is regenerated by `rackabel build`.
    let _ = std::fs::remove_file(root.join("manifest.json"));
    Ok(())
}

/// Best-effort: read the official scaffold's `manifest.json`/`package.json` to recover
/// the name/author/api-version + the `file:` dep basenames. `None` if neither is present.
fn derive_data_from_emitted(root: &Path) -> Option<ScaffoldData> {
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("manifest.json")).ok()?).ok()?;
    let pkg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("package.json")).ok()?).ok()?;

    let display_name = manifest
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("extension")
        .to_string();
    let author = manifest
        .get("author")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let api_version = manifest
        .get("minimumApiVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("1.0.0")
        .to_string();

    let dep_basename = |table: &str, key: &str| -> Option<String> {
        pkg.get(table)?
            .get(key)?
            .as_str()
            .and_then(|s| s.rsplit('/').next())
            .map(|s| s.to_string())
    };
    let sdk = dep_basename("dependencies", "@ableton-extensions/sdk")?;
    let cli = dep_basename("devDependencies", "@ableton-extensions/cli")?;

    Some(ScaffoldData {
        package_name: sanitize_package_name(&display_name),
        display_name,
        author,
        license: "MIT".to_string(),
        sdk_dep_basename: sdk,
        cli_dep_basename: cli,
        api_version,
        minimal: false,
    })
}

// --- the individual file bodies --------------------------------------------
//
// These mirror the create-extension templates (SPEC A §3.5) verbatim where the
// content is the toolchain contract (tsconfig, .env, the esbuild-relevant bits), and
// diverge only where §4.7 says rackabel must (rackabel.toml replaces manifest.json as
// the source of truth; package.json scripts call `rackabel`; .gitignore adds .rackabel/).

/// `.gitignore` — the official one (SPEC A §3.5) plus `.rackabel/` (§4.7).
fn gitignore() -> String {
    "\
.DS_Store
node_modules/
dist/

*.log
*.ablx
*.tsbuildinfo

.env

# rackabel tool state (generated)
.rackabel/
manifest.json
"
    .to_string()
}

/// `rackabel.toml` — the single source of truth (DESIGN §4.1). The display name is
/// preserved; the build/pack tables are present-but-empty so the file documents itself.
fn rackabel_toml(data: &ScaffoldData) -> String {
    // We deliberately quote the display name (it may contain spaces) and keep the
    // file minimal — every field beyond `name` has a documented inference (§4.2), so a
    // musician's file stays short. Author is only written when known.
    let mut s = String::new();
    s.push_str("# rackabel project — the single source of truth.\n");
    s.push_str("# Everything else (manifest.json, build config) is generated from this.\n\n");
    s.push_str("[extension]\n");
    s.push_str(&format!("name = {}\n", toml_quote(&data.display_name)));
    if !data.author.is_empty() {
        s.push_str(&format!("author = {}\n", toml_quote(&data.author)));
    }
    s.push_str("version = \"0.1.0\"\n");
    s.push_str(&format!(
        "minimum_api_version = {}\n",
        toml_quote(&data.api_version)
    ));
    s.push('\n');
    s.push_str(
        "# Built bundle entry is dist/extension.js (generated); source is src/extension.ts.\n",
    );
    if !data.license.is_empty() {
        s.push('\n');
        s.push_str("[meta]\n");
        s.push_str(&format!("license = {}\n", toml_quote(&data.license)));
    }
    s
}

/// `package.json` — rackabel-form (DESIGN §4.7): SDK/CLI as `file:` deps (SPEC A §3.4),
/// esbuild pinned at 0.28.0 (SPEC A §3.5), but the scripts drive `rackabel`, not
/// `tsx build.ts` / `extensions-cli`. `engines.node >=24.14.1` is the build-time floor
/// the official project bakes (DESIGN §4.2 `node_build`).
fn package_json(data: &ScaffoldData) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "name": data.package_name,
        "version": "0.1.0",
        "main": "dist/extension.js",
        "engines": { "node": ">=24.14.1" },
        "type": "module",
        "scripts": {
            "build": "rackabel build",
            "deploy": "rackabel deploy",
            "pack": "rackabel pack",
            "dev": "rackabel dev"
        },
        "dependencies": {
            "@ableton-extensions/sdk": format!("file:./vendor/{}", data.sdk_dep_basename)
        },
        "devDependencies": {
            "@ableton-extensions/cli": format!("file:./vendor/{}", data.cli_dep_basename),
            "@types/node": "^24.1.0",
            "esbuild": "0.28.0",
            "typescript": "^5.9.3"
        }
    }))
    .expect("package.json serializes")
        + "\n"
}

/// `tsconfig.json` — verbatim from the official template (SPEC A §3.5; toolchain contract).
fn tsconfig_json() -> String {
    "\
{
  \"compilerOptions\": {
    \"module\": \"nodenext\",
    \"target\": \"esnext\",
    \"moduleResolution\": \"nodenext\",
    \"outDir\": \"./dist\",
    \"rootDir\": \"./src\",
    \"strict\": true,
    \"esModuleInterop\": true,
    \"types\": [\"node\"]
  },
  \"include\": [\"src/**/*\"]
}
"
    .to_string()
}

/// `.env` — the official template (SPEC A §3.5). `EXTENSION_HOST_PATH` is left as the
/// placeholder; `rackabel dev`/`doctor` resolve the real host path. The file is gitignored.
fn env_file() -> String {
    "\
# Path to Ableton Live's Extension Host module on this machine.
# rackabel resolves this for you; set it only to override.
# This file is gitignored — each developer sets it for their own machine.
EXTENSION_HOST_PATH=/path/to/ExtensionHostNodeModule.node
"
    .to_string()
}

/// A short README pointing at the rackabel workflow.
fn readme(data: &ScaffoldData) -> String {
    format!(
        "\
# {name}

An Ableton Live Extension, scaffolded by rackabel.

## Develop

```
rackabel dev        # build, deploy into Live's User Library, and live-reload on save
```

## Ship

```
rackabel pack       # produce a distributable .ablx
```

Run `rackabel doctor` if anything looks off.
",
        name = data.display_name
    )
}

/// `src/extension.ts` — the default template adds one working right-click action plus
/// one command (DESIGN §2: "a working right-click action" + "one command"), pure-JS
/// only (no UI/vite). `--minimal` emits a bare `activate` skeleton instead.
fn extension_ts(data: &ScaffoldData) -> String {
    let id = &data.package_name;
    if data.minimal {
        // Bare skeleton: a valid activate() and nothing else (power-user start).
        return format!(
            "\
import {{ initialize, type ActivationContext }} from \"@ableton-extensions/sdk\";

export function activate(activation: ActivationContext) {{
  const context = initialize(activation, \"{api}\");
  // Register your commands and context-menu actions on `context` here.
  void context;
}}
",
            api = data.api_version
        );
    }

    // Default: a working command + an AudioClip right-click action that renames the
    // clip the action was triggered on. Adapted from the create-extension starter
    // (SPEC A §3.5), kept to pure-JS deps only (no HTML/UI import, so no vite devDep).
    // The command body uses only verified SDK API: a context-menu action on the
    // `AudioClip` scope passes the triggered object's `Handle` as the callback's first
    // argument, which `getObjectFromHandle` resolves into a typed `AudioClip`.
    format!(
        "\
import {{
  initialize,
  AudioClip,
  type ActivationContext,
  type Handle,
}} from \"@ableton-extensions/sdk\";

export function activate(activation: ActivationContext) {{
  const context = initialize(activation, \"{api}\");

  // One command: rename the audio clip the right-click action was triggered on.
  // Commands are registered in code (the SDK has no manifest-declared commands); the
  // callback receives the triggered object's Handle as its first argument.
  context.commands.registerCommand(\"{id}.renameClip\", (...args: unknown[]) => {{
    const clip = context.getObjectFromHandle(args[0] as Handle, AudioClip);
    clip.name = `${{clip.name}} (renamed)`;
    console.log(`{name}: renamed clip to ${{clip.name}}`);
  }});

  // One right-click action wired to that command, on the AudioClip scope.
  context.ui.registerContextMenuAction(
    \"AudioClip\",
    \"Rename this clip\",
    \"{id}.renameClip\",
  );
}}
",
        api = data.api_version,
        id = id,
        name = data.display_name,
    )
}

// --- helpers ---------------------------------------------------------------

/// Write `body` to `root/rel`, creating parent dirs. Framed on failure (RK1304-class —
/// a copy/write failure, build/runtime).
fn write_file(root: &Path, rel: &str, body: &str) -> CmdResult<()> {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| write_err(&path, e))?;
    }
    std::fs::write(&path, body).map_err(|e| write_err(&path, e))
}

fn write_err(path: &Path, e: std::io::Error) -> RkError {
    RkError::new(
        ErrorCode::DeployCopyFailed,
        ExitClass::BuildRuntime,
        "could not write the project files",
        "check write permissions for the target directory, then retry",
    )
    .at(path.display().to_string())
    .raw(e.into())
}

/// Minimal TOML string quoting (basic strings: escape `\` and `"`). Sufficient for
/// names/authors/versions; never used on untrusted multi-line input.
fn toml_quote(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample(minimal: bool) -> ScaffoldData {
        ScaffoldData {
            package_name: "clip-renamer".into(),
            display_name: "Clip Renamer".into(),
            author: "Jane Doe".into(),
            license: "MIT".into(),
            sdk_dep_basename: "ableton-extensions-sdk-1.0.0-beta.0.tgz".into(),
            cli_dep_basename: "ableton-extensions-cli-1.0.0-beta.0.tgz".into(),
            api_version: "1.0.0".into(),
            minimal,
        }
    }

    #[test]
    fn sanitize_matches_official_rules() {
        assert_eq!(sanitize_package_name("Clip Renamer"), "clip-renamer");
        assert_eq!(sanitize_package_name("My_Cool Ext!"), "my-cool-ext");
        assert_eq!(sanitize_package_name("--Edge--"), "edge");
        assert_eq!(sanitize_package_name("ABC123"), "abc123");
    }

    #[test]
    fn render_writes_the_rackabel_form() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        render(root, &sample(false)).unwrap();

        assert!(root.join("rackabel.toml").is_file());
        assert!(root.join("package.json").is_file());
        assert!(root.join("tsconfig.json").is_file());
        assert!(root.join(".gitignore").is_file());
        assert!(root.join(".env").is_file());
        assert!(root.join("src/extension.ts").is_file());
        // No build.ts and no hand-written manifest.json — rackabel owns those.
        assert!(!root.join("build.ts").exists());
        assert!(!root.join("manifest.json").exists());

        let toml = std::fs::read_to_string(root.join("rackabel.toml")).unwrap();
        assert!(toml.contains("name = \"Clip Renamer\""));
        assert!(toml.contains("author = \"Jane Doe\""));
        assert!(toml.contains("license = \"MIT\""));

        let pkg = std::fs::read_to_string(root.join("package.json")).unwrap();
        assert!(pkg.contains("\"name\": \"clip-renamer\""));
        assert!(pkg.contains("file:./vendor/ableton-extensions-sdk-1.0.0-beta.0.tgz"));
        assert!(pkg.contains("file:./vendor/ableton-extensions-cli-1.0.0-beta.0.tgz"));
        assert!(pkg.contains("\"esbuild\": \"0.28.0\""));
        assert!(pkg.contains("\"build\": \"rackabel build\""));

        let gi = std::fs::read_to_string(root.join(".gitignore")).unwrap();
        assert!(gi.contains(".rackabel/"));
    }

    #[test]
    fn default_template_has_command_and_menu_action() {
        let tmp = tempdir().unwrap();
        render(tmp.path(), &sample(false)).unwrap();
        let ext = std::fs::read_to_string(tmp.path().join("src/extension.ts")).unwrap();
        assert!(ext.contains("registerCommand(\"clip-renamer.renameClip\""));
        assert!(ext.contains("registerContextMenuAction("));
        assert!(ext.contains("\"Rename this clip\""));
        // pure-JS only: no HTML import / UI.
        assert!(!ext.contains("interface.html"));
    }

    #[test]
    fn minimal_is_bare_skeleton() {
        let tmp = tempdir().unwrap();
        render(tmp.path(), &sample(true)).unwrap();
        let ext = std::fs::read_to_string(tmp.path().join("src/extension.ts")).unwrap();
        assert!(ext.contains("export function activate"));
        assert!(!ext.contains("registerCommand"));
    }

    #[test]
    fn postprocess_converts_official_output() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        // Simulate an official create-extension emit.
        std::fs::write(
            root.join("manifest.json"),
            r#"{"name":"clip-renamer","author":"Jane Doe","entry":"dist/extension.js","version":"1.0.0","minimumApiVersion":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"clip-renamer","dependencies":{"@ableton-extensions/sdk":"file:./vendor/ableton-extensions-sdk-1.0.0-beta.0.tgz"},"devDependencies":{"@ableton-extensions/cli":"file:./vendor/ableton-extensions-cli-1.0.0-beta.0.tgz"}}"#,
        )
        .unwrap();
        std::fs::write(root.join("build.ts"), "// official build").unwrap();

        postprocess(root, &sample(false)).unwrap();

        assert!(!root.join("build.ts").exists());
        assert!(!root.join("manifest.json").exists());
        let toml = std::fs::read_to_string(root.join("rackabel.toml")).unwrap();
        assert!(toml.contains("name = \"clip-renamer\""));
        let pkg = std::fs::read_to_string(root.join("package.json")).unwrap();
        assert!(pkg.contains("\"build\": \"rackabel build\""));
        assert!(pkg.contains("file:./vendor/ableton-extensions-sdk-1.0.0-beta.0.tgz"));
    }
}
