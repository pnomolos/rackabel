//! The polyfill banner (DESIGN §4.6; SPEC B §1) — OWNED BY THE BUILD AGENT.
//!
//! The 1.0 Extension Host VM lacks several web globals (URL, TextEncoder, fetch's
//! Request/Response/Headers, the WHATWG stream classes, `setImmediate`,
//! `performance`). Code that touches them throws a runtime `ReferenceError` that is
//! invisible at build time. The official `create-extension` `build.ts` ships **no**
//! banner, so this is rackabel's value-add: the banner is baked into *every* build
//! (including `--release`), which is precisely why `doctor` reports the "forgotten
//! polyfill banner" footgun as impossible (DESIGN §6.3).
//!
//! The constant below is byte-identical to `EH_POLYFILL_BANNER` from
//! `ableton-extensions-public/scripts/build-extension.js` (SPEC B §1, the canonical
//! public variant). Every polyfill is guarded by `typeof X === "undefined"`, so
//! baking it is safe even on a future host that gains these globals. Do NOT edit the
//! string casually — `Request`/`Response`/`Headers` deliberately use
//! `_ehVm.runInThisContext(...)`, the stream classes use `stream/web`
//! (`_ehWeb`), and `performance` uses `perf_hooks` (`_ehPerf`).

/// The esbuild `banner.js` value, injected unconditionally into every Extension
/// build (DESIGN §4.6; SPEC B §1). Byte-identical to the canonical public
/// `EH_POLYFILL_BANNER` (its `.trim()`-ed form: no leading/trailing blank line).
pub const POLYFILL_BANNER: &str = r#"var _ehUrl=require("url"),_ehUtil=require("util"),_ehBuf=require("buffer"),_ehVm=require("vm"),_ehWeb=require("stream/web"),_ehPerf=require("perf_hooks");
if(typeof URL==="undefined")globalThis.URL=_ehUrl.URL;
if(typeof URLSearchParams==="undefined")globalThis.URLSearchParams=_ehUrl.URLSearchParams;
if(typeof TextEncoder==="undefined")globalThis.TextEncoder=_ehUtil.TextEncoder;
if(typeof TextDecoder==="undefined")globalThis.TextDecoder=_ehUtil.TextDecoder;
if(typeof atob==="undefined")globalThis.atob=_ehBuf.atob;
if(typeof btoa==="undefined")globalThis.btoa=_ehBuf.btoa;
if(typeof Request==="undefined")globalThis.Request=_ehVm.runInThisContext("Request");
if(typeof Response==="undefined")globalThis.Response=_ehVm.runInThisContext("Response");
if(typeof Headers==="undefined")globalThis.Headers=_ehVm.runInThisContext("Headers");
if(typeof ReadableStream==="undefined")globalThis.ReadableStream=_ehWeb.ReadableStream;
if(typeof WritableStream==="undefined")globalThis.WritableStream=_ehWeb.WritableStream;
if(typeof TransformStream==="undefined")globalThis.TransformStream=_ehWeb.TransformStream;
if(typeof setImmediate==="undefined")globalThis.setImmediate=function(cb){return setTimeout(cb,0)};
if(typeof clearImmediate==="undefined")globalThis.clearImmediate=clearTimeout;
if(typeof performance==="undefined")globalThis.performance=_ehPerf.performance;"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_is_byte_identical_to_spec_b() {
        // Spot-check the load-bearing facts from SPEC B §1: the exact require
        // bindings, the vm-based fetch globals, the stream/web classes, perf_hooks,
        // and clearImmediate (included even though DESIGN §4.6 omits it).
        assert!(POLYFILL_BANNER.starts_with(
            "var _ehUrl=require(\"url\"),_ehUtil=require(\"util\"),_ehBuf=require(\"buffer\"),\
             _ehVm=require(\"vm\"),_ehWeb=require(\"stream/web\"),_ehPerf=require(\"perf_hooks\");"
        ));
        assert!(POLYFILL_BANNER.contains("_ehVm.runInThisContext(\"Request\")"));
        assert!(POLYFILL_BANNER.contains("_ehVm.runInThisContext(\"Response\")"));
        assert!(POLYFILL_BANNER.contains("_ehVm.runInThisContext(\"Headers\")"));
        assert!(POLYFILL_BANNER.contains("globalThis.ReadableStream=_ehWeb.ReadableStream"));
        assert!(POLYFILL_BANNER.contains("globalThis.performance=_ehPerf.performance"));
        assert!(POLYFILL_BANNER.contains("globalThis.clearImmediate=clearTimeout"));
        // No leading/trailing blank line (the .trim()-ed form).
        assert!(!POLYFILL_BANNER.starts_with('\n'));
        assert!(!POLYFILL_BANNER.ends_with('\n'));
        assert_eq!(POLYFILL_BANNER.lines().count(), 16);
    }
}
