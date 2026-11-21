//! Screen mirroring and control, built in: the scrcpy server runs on the phone
//! and this module speaks its protocol (see `assets/scrcpy/NOTICE.md`).

pub mod decode;
pub mod proto;
pub mod session;

pub use decode::Frame;
pub use proto::{Control, KeyAction, ServerOptions, TouchAction};
pub use session::{Event, Session};

/// A random 31-bit session id; two sessions on one device never share a socket name.
pub fn fresh_scid() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    // `RandomState` is seeded from the OS, which is all the entropy this needs.
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.write_u32(std::process::id());
    (h.finish() as u32) & 0x7fff_ffff
}

#[cfg(test)]
mod tests {
    #[test]
    fn session_ids_are_31_bit_and_vary() {
        let ids: Vec<u32> = (0..8).map(|_| super::fresh_scid()).collect();
        assert!(ids.iter().all(|&i| i < 1 << 31));
        assert!(ids.iter().any(|&i| i != ids[0]), "{ids:?}");
    }
}
