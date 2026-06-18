//! Random hex identifiers, sourced from the system RNG.

use std::io::Read;

/// A random lowercase-hex string of `bytes` random bytes (so `2 * bytes` chars).
pub fn random_hex(bytes: usize) -> std::io::Result<String> {
    let mut urandom = std::fs::File::open("/dev/urandom")?;
    let mut buf = vec![0u8; bytes];
    urandom.read_exact(&mut buf)?;
    let mut hex = String::with_capacity(bytes * 2);
    for byte in buf {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// A fresh entity id (128 bits of randomness, hex-encoded).
pub fn new_id() -> std::io::Result<String> {
    random_hex(16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_hex_and_unique() {
        let a = new_id().expect("urandom");
        let b = new_id().expect("urandom");
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
