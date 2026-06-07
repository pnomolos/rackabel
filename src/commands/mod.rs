pub mod build;
pub mod deploy;
// The managed dev host command group (milestone 0.3). Unix-only (the daemon uses
// setsid/setpgid/killpg); on Windows the dispatch returns a clean RK0307 (SPEC D §9.3).
#[cfg(unix)]
pub mod dev;
pub mod doctor;
pub mod explain;
pub mod install;
pub mod new;
pub mod pack;
/// rackabel's own third-party plugin group (milestone 0.4, DESIGN §5). Platform-
/// independent (no daemon mechanics), so — unlike `dev` — it is NOT `#[cfg(unix)]`.
pub mod plugin;
pub mod validate;
pub mod watch;
