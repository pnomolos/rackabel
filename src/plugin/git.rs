//! A thin, arg-array-safe wrapper over the system `git` binary (DESIGN §5.4/§5.5).
//!
//! FOUNDATION-OWNED. Both `plugin install OWNER/REPO` (clone + build) and
//! `new --template gh:…` (clone the template repo) need: shallow-clone a repo at a ref,
//! and read the resolved commit (rev-parse HEAD) to PIN it. Every invocation passes args
//! as a fixed array (never a shell string), so a URL/ref with shell metacharacters can't
//! inject — the template/plugin source is third-party input.
//!
//! No network mocking lives here: tests exercise it against LOCAL git repos created in
//! tempdirs (and `file://` remotes), never the network. The remote URL the caller passes
//! may be rewritten by the [`super::source`] seam (`RACKABEL_TEMPLATE_GIT_BASE`) so a
//! `gh:` ref resolves to a local `file://` base in tests.

use std::path::Path;

use crate::error::{CmdResult, ErrorCode, RkError};

/// Shallow-clone `url` (optionally at `git_ref`) into `dest` with `--depth 1`. `dest`
/// must not already exist (git's own requirement). On failure returns a framed error;
/// `code`/`class` are chosen by the caller (a plugin install vs a template fetch differ
/// in remedy). The args are a fixed array — `url`/`git_ref` are never shell-interpolated.
pub fn clone_shallow(
    url: &str,
    git_ref: Option<&str>,
    dest: &Path,
    code: ErrorCode,
) -> CmdResult<()> {
    let dest_str = dest.to_string_lossy();
    let mut args: Vec<&str> = vec!["clone", "--depth", "1", "--quiet"];
    if let Some(r) = git_ref {
        args.push("--branch");
        args.push(r);
    }
    args.push(url);
    args.push(&dest_str);

    run(&args, None, code, "could not clone the repository")?;
    Ok(())
}

/// Clone `url` into `dest` with FULL history (no `--depth`), optionally at `git_ref`.
/// Unlike [`clone_shallow`], the resulting work tree can be `git checkout`'d to an
/// arbitrary historical commit — which `new --update` needs to re-render the OLD baseline
/// (template@oldcommit) and the NEW render (template@newcommit) from one clone. The args
/// are a fixed array; `url`/`git_ref` are never shell-interpolated.
pub fn clone_full(url: &str, git_ref: Option<&str>, dest: &Path, code: ErrorCode) -> CmdResult<()> {
    let dest_str = dest.to_string_lossy();
    let mut args: Vec<&str> = vec!["clone", "--quiet"];
    if let Some(r) = git_ref {
        args.push("--branch");
        args.push(r);
    }
    args.push(url);
    args.push(&dest_str);
    run(&args, None, code, "could not clone the repository")?;
    Ok(())
}

/// Check out `commit` (a sha/ref) in the work tree at `repo` (detached). Used by
/// `new --update` to materialize a template at a specific historical commit.
pub fn checkout(repo: &Path, commit: &str, code: ErrorCode) -> CmdResult<()> {
    run(
        &["checkout", "--quiet", commit],
        Some(repo),
        code,
        "could not check out that commit",
    )?;
    Ok(())
}

/// 3-way merge `current` (ours) against `base` and `other` (theirs) IN PLACE, writing
/// conflict markers into `current` on a conflict (the `git merge-file` contract). Returns
/// `true` on a clean merge, `false` when conflict markers were written. A genuine error
/// (a missing file, git not runnable) is framed; the merge "conflict" exit (>0 but not an
/// error) is mapped to `false`, not an error.
pub fn merge_file(current: &Path, base: &Path, other: &Path, code: ErrorCode) -> CmdResult<bool> {
    let cur = current.to_string_lossy();
    let bas = base.to_string_lossy();
    let oth = other.to_string_lossy();
    // -p would write to stdout; we want the in-place 3-way merge of `current`.
    // Labels make the conflict markers self-explaining in the user's file.
    let args = vec![
        "merge-file",
        "-L",
        "your version",
        "-L",
        "template (old)",
        "-L",
        "template (new)",
        cur.as_ref(),
        bas.as_ref(),
        oth.as_ref(),
    ];
    let mut cmd = std::process::Command::new("git");
    cmd.args(&args);
    let output = cmd.output().map_err(|e| {
        RkError::of(
            code,
            "could not run `git merge-file`",
            "install git (it ships with the Xcode command-line tools on macOS), then retry",
        )
        .raw(e.into())
    })?;
    // git merge-file: exit 0 = clean, exit N>0 (number of conflicts) = conflicts written,
    // exit <0 / 255 = real error.
    match output.status.code() {
        Some(0) => Ok(true),
        Some(n) if n > 0 && n < 128 => Ok(false),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            Err(RkError::of(
                code,
                "git merge-file failed",
                "run with --raw for git's output",
            )
            .raw(anyhow::anyhow!(stderr)))
        }
    }
}

/// The resolved commit of the work tree at `repo` (`git rev-parse HEAD`). This is the
/// value pinned in `plugins.lock` / `.rackabel-template` — a 40-hex sha.
pub fn rev_parse_head(repo: &Path, code: ErrorCode) -> CmdResult<String> {
    let out = run(
        &["rev-parse", "HEAD"],
        Some(repo),
        code,
        "could not read the repository's commit",
    )?;
    Ok(out.trim().to_string())
}

/// Initialize a fresh repo at `dir` (used by tests to build local fixture repos; also a
/// harmless utility). Best-effort framing.
pub fn init(dir: &Path, code: ErrorCode) -> CmdResult<()> {
    run(
        &["init", "--quiet"],
        Some(dir),
        code,
        "could not initialize a git repository",
    )?;
    Ok(())
}

/// Run `git <args>` (optionally in `cwd`), returning stdout on success. A non-zero exit
/// or a spawn failure is framed with `code`. stderr is attached to the raw chain
/// (`--raw`/`--verbose`).
fn run(args: &[&str], cwd: Option<&Path>, code: ErrorCode, problem: &str) -> CmdResult<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let output = cmd.output().map_err(|e| {
        RkError::of(
            code,
            "could not run `git`",
            "install git (it ships with the Xcode command-line tools on macOS), then retry",
        )
        .at(format!("git {}", args.join(" ")))
        .raw(e.into())
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(RkError::of(
            code,
            problem.to_string(),
            "check the ref/URL and your access, then retry (run with --raw for git's output)",
        )
        .at(format!("git {}", args.join(" ")))
        .raw(anyhow::anyhow!(stderr)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// True iff a real `git` binary is on PATH (the tests are skipped otherwise so CI
    /// without git stays green — the wrapper's framing is exercised by the missing-binary
    /// path elsewhere).
    fn has_git() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Build a local fixture repo with one commit and return (repo_dir, commit).
    fn fixture_repo(dir: &Path) -> String {
        init(dir, ErrorCode::TemplateNotFound).unwrap();
        // identity so commit succeeds in a clean CI env
        for kv in [("user.email", "t@t"), ("user.name", "t")] {
            std::process::Command::new("git")
                .args(["config", kv.0, kv.1])
                .current_dir(dir)
                .output()
                .unwrap();
        }
        std::fs::write(dir.join("rackabel-template.toml"), "[prompts]\n").unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-m", "init", "--quiet"]] {
            let st = std::process::Command::new("git")
                .args(&args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(st.status.success(), "git {:?} failed", args);
        }
        rev_parse_head(dir, ErrorCode::TemplateNotFound).unwrap()
    }

    #[test]
    fn clone_local_repo_and_pin_commit() {
        if !has_git() {
            return;
        }
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let commit = fixture_repo(&src);

        // Clone the local repo via a file:// URL into a fresh dest.
        let dest = tmp.path().join("dest");
        let url = format!("file://{}", src.display());
        clone_shallow(&url, None, &dest, ErrorCode::TemplateNotFound).unwrap();
        assert!(dest.join("rackabel-template.toml").is_file());

        let cloned_commit = rev_parse_head(&dest, ErrorCode::TemplateNotFound).unwrap();
        assert_eq!(cloned_commit, commit);
        assert_eq!(commit.len(), 40);
    }

    #[test]
    fn clone_bad_url_is_framed_with_caller_code() {
        if !has_git() {
            return;
        }
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join("dest");
        let err = clone_shallow(
            "file:///definitely/not/a/repo/xyz",
            None,
            &dest,
            ErrorCode::PluginNotFound,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::PluginNotFound);
    }
}
