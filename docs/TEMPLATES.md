# Authoring a rackabel template (tier-1)

A **template** is the lowest-effort way to extend rackabel (DESIGN §5.5): a git repo (or a
local directory) that `rackabel new --template <ref>` renders into a fresh project. A
template is **declarative data only** — it never depends on rackabel internals, so it does
not bit-rot when rackabel changes.

```
rackabel new my-project --template gh:owner/repo          # a GitHub repo
rackabel new my-project --template gh:owner/repo@v2        # at a tag/branch
rackabel new my-project --template ./path/to/template      # a local directory
```

## Anatomy

A directory is a template iff it has a **`rackabel-template.toml`** at its root. Everything
else in the directory is copied into the new project (with placeholder substitution), except:

- `rackabel-template.toml` itself (the manifest is not copied),
- any `.rackabel-template` lockfile (rackabel writes a fresh one),
- the `.git/` directory.

## `rackabel-template.toml`

```toml
# [prompts] — each key is a placeholder name; rackabel renders these as the `new` wizard,
# with the bracketed default accepted on Enter.
[prompts.name]
label   = "Extension name"   # the prompt text (defaults to the key)
type    = "string"            # string | bool | choice
default = "my-ext"            # seeds the bracketed default (a bool default is "true"/"false")

[prompts.flavor]
type    = "choice"
choices = ["plain", "fancy"]  # required for a choice; the default must be one of them
default = "plain"

[prompts.git]
type    = "bool"
default = "true"

# [merge] — controls `new --update`'s 3-way merge.
[merge]
# Globs (relative to the project root) excluded from the TEXT merge. Use this for binary or
# generated files. NOTE: vendored tarballs and common binaries are ALWAYS excluded even if
# you don't list them (see below), so you only need to list your own author-editable-but-
# -don't-merge files here.
exclude = ["docs/generated/**"]
```

## Placeholder syntax

The substitution language is intentionally tiny — **one construct**:

```
{{ key }}
```

- A `{{ key }}` token (inner whitespace optional: `{{key}}`, `{{ key }}`, `{{  key  }}` are
  equivalent) is replaced by the answer for `key`.
- An **unknown** key (no matching prompt/answer) is left **verbatim** — a typo shows up in
  the output instead of silently vanishing, and a literal `{{ … }}` you want to keep
  survives.
- Substitution is a **single pass**: a replacement value is never re-scanned, so an answer
  that itself contains `{{ … }}` can never trigger a second substitution.

There are no conditionals, loops, partials, or filters — by design. Keep templates as data.

## `new --update` (keeping a project current with its template)

A project scaffolded from a template carries a `.rackabel-template` lockfile recording the
template repo + ref + resolved commit + the answers you gave. Later:

```
rackabel new --update            # re-render the template at its new commit and 3-way-merge
rackabel new --update --dry-run  # show the plan (apply / conflict / overwrite / skip), do nothing
```

`--update` re-renders the **old** baseline (template@oldcommit + your saved answers) and the
**new** version (template@newcommit + the same answers, prompting only for prompts that are
**new** in the updated template), then 3-way-merges against your working tree:

- files that changed only in the template apply **silently**;
- files that changed in both get conflict markers (`<<<<<<< / ======= / >>>>>>>`) and a
  summary `help:` line (exit code 4, `RK4008`) — you resolve them by hand;
- files in `[merge].exclude` (and the always-excluded set below) are **never** text-merged —
  overwritten from the new render when changed, or left alone.

`--update` is an explicit developer action; it never runs on the no-flag happy path and never
silently clobbers a setup.

### Always-excluded files

These are excluded from the text merge regardless of `[merge].exclude`, because a
marker-based merge can't reconcile their bytes (they differ by SDK/asset version, not
template commit):

```
**/*.tgz  **/*.tar.gz  **/*.tar  **/*.zip  **/*.ablx  **/*.amxd
**/*.node  **/*.wasm  **/*.png  **/*.jpg  **/*.jpeg  **/*.gif  **/*.ico
**/vendor/**  **/node_modules/**
**/package-lock.json  **/pnpm-lock.yaml  **/yarn.lock
```

## Remote templates are unreviewed third-party code

A **remote** template (`gh:…` / `@scope/…`) is fetched and then **built** by `new`'s
auto-build, running its build configuration with your full privileges. rackabel prints the
resolved repo/ref and a warning and **requires confirmation** before fetching (`--yes` to
consent in a script; `--no-input` refuses rather than silently proceeding). Local paths and
the built-in default skip this prompt. See DESIGN §5.7.

> Scoped `@scope/name` templates are accepted and classified but **not resolved yet** in this
> release — point `--template` at the GitHub repo (`gh:owner/repo`) or a local checkout.
