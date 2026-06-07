//! The template placeholder substitution syntax (DESIGN §5.5).
//!
//! Deliberately SIMPLE and deterministic — no logic, no loops, no partials — so a
//! template is "declarative data, never dependent on rackabel internals" and can't
//! bit-rot (§5.5). The single construct is:
//!
//! ```text
//! {{ key }}
//! ```
//!
//! A `{{ key }}` token (optional surrounding ASCII whitespace inside the braces) is
//! replaced by the answer for `key`. An UNKNOWN key (no answer) is left VERBATIM rather
//! than substituted-to-empty, so a typo is visible in the output instead of silently
//! vanishing, and a literal `{{ … }}` the author wanted to keep survives. Substitution is
//! a single left-to-right pass: replacement text is NOT re-scanned, so an answer that
//! itself contains `{{ … }}` can never trigger a second substitution (no surprises, no
//! injection through answer values).
//!
//! This is the syntax documented for template authors in `docs/TEMPLATES.md` (the
//! template-author reference) — kept in lock-step with this module.

use std::collections::BTreeMap;

/// Render `input`, replacing every `{{ key }}` token with `answers[key]`. Unknown keys are
/// left verbatim. Single pass (replacements are not re-scanned).
pub fn render(input: &str, answers: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Look for the opening `{{`.
        if bytes[i] == b'{'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'{'
            && let Some((key, end)) = parse_token(input, i)
        {
            match answers.get(&key) {
                Some(val) => out.push_str(val),
                // Unknown key: emit the token verbatim.
                None => out.push_str(&input[i..end]),
            }
            i = end;
            continue;
        }
        // Not a token start: copy the char (use char boundary-safe slicing).
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&input[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Parse a `{{ key }}` token starting at `start` (which points at the first `{`). Returns
/// `(trimmed_key, end_index_after_closing_braces)` if it is a well-formed token whose key
/// is a single identifier-ish run (no `{`/`}` inside), else `None`.
fn parse_token(input: &str, start: usize) -> Option<(String, usize)> {
    let rest = &input[start + 2..];
    let close = rest.find("}}")?;
    let inner = &rest[..close];
    // Reject a token containing a brace (e.g. `{{ {{x}} }}`) — keep parsing simple.
    if inner.contains('{') || inner.contains('}') {
        return None;
    }
    let key = inner.trim();
    if key.is_empty() {
        return None;
    }
    let end = start + 2 + close + 2;
    Some((key.to_string(), end))
}

/// Byte length of the UTF-8 sequence whose lead byte is `b`.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn substitutes_known_keys() {
        let a = answers(&[("name", "clip-renamer"), ("author", "Jane")]);
        assert_eq!(
            render("# {{ name }} by {{ author }}", &a),
            "# clip-renamer by Jane"
        );
    }

    #[test]
    fn whitespace_inside_braces_is_optional() {
        let a = answers(&[("x", "1")]);
        assert_eq!(render("{{x}}/{{ x }}/{{  x  }}", &a), "1/1/1");
    }

    #[test]
    fn unknown_key_left_verbatim() {
        let a = answers(&[("name", "v")]);
        assert_eq!(render("{{ name }} {{ missing }}", &a), "v {{ missing }}");
    }

    #[test]
    fn replacement_is_not_rescanned() {
        // An answer containing a token must NOT trigger a second substitution.
        let a = answers(&[("a", "{{ b }}"), ("b", "BOOM")]);
        assert_eq!(render("{{ a }}", &a), "{{ b }}");
    }

    #[test]
    fn unicode_is_preserved() {
        let a = answers(&[("name", "café")]);
        assert_eq!(render("héllo {{ name }} ☃", &a), "héllo café ☃");
    }

    #[test]
    fn lone_braces_are_literal() {
        let a = answers(&[]);
        assert_eq!(render("a { b } c {{}} d", &a), "a { b } c {{}} d");
    }
}
