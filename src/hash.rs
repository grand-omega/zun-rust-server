use std::fmt::Write;

use sha2::{Digest, Sha256};

/// Lower-case hex sha256 of `bytes`. 64 chars.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in digest.iter() {
        write!(&mut s, "{b:02x}").expect("write to String never fails");
    }
    s
}

/// Validate that a string is a 64-char lowercase hex sha256.
pub fn is_valid_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
