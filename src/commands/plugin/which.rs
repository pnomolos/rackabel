//! `rackabel plugin which <name>` (DESIGN §5.4/§5.6) — FOUNDATION-OWNED.
//!
//! Reports exactly which file a name would run, or "shadowed by built-in" with a pointer
//! to `plugin run`. A pure read over [`crate::plugin::resolve`] — works on day one and
//! pre-empts cargo's ambiguous-shadowing pain (#6507).

use crate::cli::PluginNameArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::plugin::plugins_bin_dir;
use crate::plugin::resolve::{self, Resolution};

pub fn run(args: &PluginNameArgs, ctx: &Ctx) -> CmdResult<()> {
    let name = &args.name;
    let r = resolve::resolve_real(ctx, name);

    if ctx.json {
        print_json(name, &r);
        // A shadowed/not-found state still exits with the relevant code even under
        // --json, so a script can branch on the exit code as well as the JSON.
        return classify_exit(name, &r);
    }

    match &r {
        Resolution::Builtin { shadowed_plugin } => {
            match shadowed_plugin {
                Some(p) => {
                    println!("{name}: shadowed by built-in (plugin at {})", p.display());
                }
                None => println!("{name}: built-in subcommand"),
            }
            classify_exit(name, &r)
        }
        Resolution::Managed { path, also_on_path } => {
            println!("{name}: {} (managed)", path.display());
            if *also_on_path && ctx.echo_on() {
                println!(
                    "  note: also on $PATH; the managed copy in {} wins",
                    plugins_bin_dir(ctx).display()
                );
            }
            Ok(())
        }
        Resolution::Path { path } => {
            println!("{name}: {} ($PATH)", path.display());
            Ok(())
        }
        Resolution::NotFound => classify_exit(name, &r),
    }
}

/// Turn a resolution into the appropriate exit: shadowed → RK0103, not-found → RK0401,
/// runnable → Ok.
fn classify_exit(name: &str, r: &Resolution) -> CmdResult<()> {
    match r {
        Resolution::Builtin {
            shadowed_plugin: Some(_),
        } => Err(RkError::of(
            ErrorCode::PluginShadowedByBuiltin,
            format!("`{name}` is claimed by a built-in; the plugin is shadowed"),
            format!("run the plugin anyway with `rackabel plugin run {name}`"),
        )),
        // A built-in with no plugin is not an error — it's just a built-in.
        Resolution::Builtin {
            shadowed_plugin: None,
        } => Ok(()),
        Resolution::NotFound => Err(RkError::of(
            ErrorCode::PluginNotFound,
            format!("no plugin named `{name}` is installed or on PATH"),
            "run `rackabel plugin list`, or `rackabel plugin install OWNER/REPO`",
        )),
        _ => Ok(()),
    }
}

fn print_json(name: &str, r: &Resolution) {
    let obj = match r {
        Resolution::Builtin { shadowed_plugin } => serde_json::json!({
            "name": name,
            "resolution": "builtin",
            "shadowed_plugin": shadowed_plugin.as_ref().map(|p| p.display().to_string()),
        }),
        Resolution::Managed { path, also_on_path } => serde_json::json!({
            "name": name,
            "resolution": "managed",
            "path": path.display().to_string(),
            "also_on_path": also_on_path,
        }),
        Resolution::Path { path } => serde_json::json!({
            "name": name,
            "resolution": "path",
            "path": path.display().to_string(),
        }),
        Resolution::NotFound => serde_json::json!({
            "name": name,
            "resolution": "not_found",
        }),
    };
    println!("{}", serde_json::to_string_pretty(&obj).unwrap());
}
