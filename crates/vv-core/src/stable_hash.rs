use std::hash::{Hash, Hasher};

/// FNV-1a: the same hash in every build, for the keys kept on disk.
#[derive(Default)]
pub struct StableHasher(Option<u64>);

impl Hasher for StableHasher {
    fn write(&mut self, bytes: &[u8]) {
        let mut hash = self.0.unwrap_or(0xcbf2_9ce4_8422_2325);
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
        self.0 = Some(hash);
    }

    fn finish(&self) -> u64 {
        self.0.unwrap_or(0xcbf2_9ce4_8422_2325)
    }
}

pub fn stable_hash(value: impl Hash) -> u64 {
    let mut h = StableHasher::default();
    value.hash(&mut h);
    h.finish()
}

#[cfg(test)]
#[path = "tests/stable_hash.rs"]
mod tests;
