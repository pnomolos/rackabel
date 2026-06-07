//! Single source of truth for the user-facing constants `rackabel new` prints.
//!
//! DESIGN §6.2 is explicit that the Extensions-beta URL is a *placeholder to confirm
//! at ship*, and that it must come from "remote/updatable config — **not** a
//! hard-coded constant scattered through the code" so a moved page can be corrected
//! without re-touching every call site. For 0.2 there is no remote-config fetch yet
//! (that lands with the dev-host/registry milestone), so we centralize the value in
//! exactly one place: this module. Every command that needs the URL reads it from
//! here, so updating one constant updates the whole tool. A `RACKABEL_EXTENSIONS_BETA_URL`
//! env override is honored so the page can be corrected without a release in the
//! interim, satisfying the "updatable, not hard-coded" intent.

/// The Extensions beta enrollment / download page (placeholder — confirm at ship).
pub const EXTENSIONS_BETA_URL_DEFAULT: &str = "https://www.ableton.com/extensions-beta";

/// Where to get Ableton Live itself (the "no Live" aside in §6.2).
pub const LIVE_DOWNLOAD_URL: &str = "https://www.ableton.com/live";

/// The env override that lets the beta URL be corrected without a rackabel release
/// (the "updatable config" intent of §6.2, ahead of a real remote-config fetch).
const BETA_URL_ENV: &str = "RACKABEL_EXTENSIONS_BETA_URL";

/// The resolved Extensions beta URL: the env override if set and non-empty, else the
/// shipped default. This is the *one* place the URL is sourced.
pub fn extensions_beta_url() -> String {
    match std::env::var(BETA_URL_ENV) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => EXTENSIONS_BETA_URL_DEFAULT.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_url_when_env_unset() {
        // SAFETY: single-threaded test; we remove the var to assert the default.
        unsafe { std::env::remove_var(BETA_URL_ENV) };
        assert_eq!(extensions_beta_url(), EXTENSIONS_BETA_URL_DEFAULT);
    }
}
