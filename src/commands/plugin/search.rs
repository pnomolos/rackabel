//! `rackabel plugin search <term>` (DESIGN §5.4).
//!
//! OWNED BY THE PLUGIN-MGMT AGENT. Queries the `rackabel-plugin` GitHub topic via the REST
//! API behind the [`crate::plugin::source::github_api_base`] seam (tests stub the base URL;
//! no live network in tests). A no-network / rate-limit failure is the clean `RK0404`
//! frame; `--json` is the machine-readable result surface (§7).

use crate::cli::PluginSearchArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::plugin::source::github_api_base;

/// One search hit (a repo tagged `rackabel-plugin` matching the term).
struct Hit {
    full_name: String,
    description: String,
    stars: i64,
    url: String,
}

pub fn run(args: &PluginSearchArgs, ctx: &Ctx) -> CmdResult<()> {
    // A plugin command: surface any upgrade-time collision loudly once (§5.6).
    crate::plugin::collision::check_and_warn(ctx, crate::cli::is_reserved);

    let base = github_api_base();
    // GET <base>/search/repositories?q=topic:rackabel-plugin+<term>&sort=stars
    let q = format!("topic:rackabel-plugin {}", args.term);
    let url = format!(
        "{}/search/repositories?q={}&sort=stars&order=desc",
        base.trim_end_matches('/'),
        urlencode(&q)
    );

    let body = http_get_string(&url)?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        RkError::of(
            ErrorCode::NoNetwork,
            "the GitHub search response could not be parsed",
            "retry shortly; if it persists it may be a rate limit",
        )
        .at(url.clone())
        .raw(e.into())
    })?;

    let hits = parse_hits(&json);

    if ctx.json {
        let arr: Vec<_> = hits
            .iter()
            .map(|h| {
                serde_json::json!({
                    "full_name": h.full_name,
                    "description": h.description,
                    "stars": h.stars,
                    "url": h.url,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "term": args.term,
                "results": arr,
            }))
            .unwrap()
        );
        return Ok(());
    }

    if hits.is_empty() {
        println!(
            "no plugins matching `{}` in the rackabel-plugin topic",
            args.term
        );
        println!("  publish one by tagging your repo with the `rackabel-plugin` GitHub topic");
        return Ok(());
    }

    for h in &hits {
        let desc = if h.description.is_empty() {
            String::new()
        } else {
            format!(" — {}", h.description)
        };
        println!("{}  (★{}){}", h.full_name, h.stars, desc);
        println!("  install: rackabel plugin install {}", h.full_name);
    }
    Ok(())
}

fn parse_hits(json: &serde_json::Value) -> Vec<Hit> {
    json.get("items")
        .and_then(|i| i.as_array())
        .map(|items| {
            items
                .iter()
                .map(|it| Hit {
                    full_name: it
                        .get("full_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    description: it
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    stars: it
                        .get("stargazers_count")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0),
                    url: it
                        .get("html_url")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Minimal percent-encoding for a query string (space → `+`, the rest of the unreserved
/// set passes through; `:` is kept readable for the `topic:` qualifier). Enough for the
/// GitHub search `q=`; avoids pulling a URL-encoding crate.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b' ' => out.push('+'),
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn http_get_string(url: &str) -> CmdResult<String> {
    // Reuse the timeout-configured agent so a stalled GitHub search connection surfaces as
    // RK0404 rather than hanging forever (ureq applies no timeout unless one is set).
    match crate::plugin::store::http_agent()
        .get(url)
        .set("User-Agent", "rackabel")
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(resp) => resp.into_string().map_err(|e| {
            RkError::of(
                ErrorCode::NoNetwork,
                "could not read the search response body",
                "retry shortly, or browse the `rackabel-plugin` GitHub topic in a browser",
            )
            .at(url.to_string())
            .raw(e.into())
        }),
        Err(ureq::Error::Status(code, _)) if code == 403 || code == 429 => Err(RkError::of(
            ErrorCode::NoNetwork,
            "the GitHub search API rate-limited the request",
            "wait a few minutes and retry, or set GITHUB_TOKEN for a higher limit",
        )
        .at(url.to_string())),
        Err(ureq::Error::Status(code, _)) => Err(RkError::of(
            ErrorCode::NoNetwork,
            format!("the GitHub search request failed (HTTP {code})"),
            "retry shortly, or browse the `rackabel-plugin` GitHub topic in a browser",
        )
        .at(url.to_string())),
        Err(e @ ureq::Error::Transport(_)) => Err(RkError::of(
            ErrorCode::NoNetwork,
            "could not reach the network to search GitHub",
            "check your connection and retry, or sideload a local path/tarball instead",
        )
        .at(url.to_string())
        .raw(e.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hits_reads_items() {
        let json = serde_json::json!({
            "items": [
                { "full_name": "acme/rackabel-notarize", "description": "Notarize", "stargazers_count": 12, "html_url": "https://x/1" },
                { "full_name": "b/rackabel-foo", "stargazers_count": 3, "html_url": "https://x/2" }
            ]
        });
        let hits = parse_hits(&json);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].full_name, "acme/rackabel-notarize");
        assert_eq!(hits[0].stars, 12);
        // Missing description defaults to empty.
        assert_eq!(hits[1].description, "");
    }

    #[test]
    fn parse_hits_empty_when_no_items() {
        assert!(parse_hits(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn urlencode_space_and_qualifier() {
        assert_eq!(
            urlencode("topic:rackabel-plugin midi"),
            "topic:rackabel-plugin+midi"
        );
        assert_eq!(urlencode("a/b"), "a%2Fb");
    }
}
