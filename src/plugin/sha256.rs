//! A small sha256 helper for plugin pinning (DESIGN §5.4).
//!
//! FOUNDATION-OWNED. `plugin install` of a downloaded asset / sideloaded tarball/path
//! pins by the sha256 of the resolved bytes; install/verify compares against the pin
//! (`RK4007` on mismatch). Pure-Rust (`sha2`), no system/openssl dependency.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{CmdResult, ErrorCode, RkError};

/// Hex sha256 of a byte slice.
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

/// Hex sha256 of a file's contents. Reads the whole file (plugin assets are small).
pub fn hash_file(path: &Path) -> CmdResult<String> {
    let bytes = std::fs::read(path).map_err(|e| {
        RkError::of(
            ErrorCode::PinMismatch,
            "could not read the file to hash it",
            "check the path and its permissions, then retry",
        )
        .at(path.display().to_string())
        .raw(e.into())
    })?;
    Ok(hash_bytes(&bytes))
}

/// Verify a file matches an expected hex sha256 pin. `RK4007 PinMismatch` on a mismatch
/// (validation, exit 4) so CI can gate deterministically. The comparison is
/// case-insensitive on the hex.
pub fn verify_file(path: &Path, expected: &str) -> CmdResult<()> {
    let actual = hash_file(path)?;
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        Err(RkError::of(
            ErrorCode::PinMismatch,
            "the installed file does not match its pinned sha256",
            "re-run the install to fetch the pinned bytes, or pass --force to update \
             past the pin (it will announce the change)",
        )
        .at(format!(
            "{}\n  expected sha256 {expected}\n  actual   sha256 {actual}",
            path.display()
        )))
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn known_vector() {
        // sha256("abc") — a standard test vector.
        assert_eq!(
            hash_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // sha256("") — the empty input.
        assert_eq!(
            hash_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn file_hash_and_verify() {
        let tmp = tempdir().unwrap();
        let f = tmp.path().join("asset");
        std::fs::write(&f, b"abc").unwrap();
        let h = hash_file(&f).unwrap();
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Verify passes for the right pin (case-insensitive) and fails (RK4007) otherwise.
        verify_file(&f, &h.to_uppercase()).unwrap();
        let err = verify_file(&f, "deadbeef").unwrap_err();
        assert_eq!(err.code, ErrorCode::PinMismatch);
    }
}
