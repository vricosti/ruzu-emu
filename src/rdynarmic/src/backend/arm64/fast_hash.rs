use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default)]
pub struct FastHasher(u64);

impl Hasher for FastHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = if self.0 == 0 {
            0xcbf2_9ce4_8422_2325u64
        } else {
            self.0
        };
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = hash;
    }

    fn write_u32(&mut self, i: u32) {
        self.write(&i.to_ne_bytes());
    }

    fn write_u64(&mut self, i: u64) {
        self.write(&i.to_ne_bytes());
    }

    fn write_usize(&mut self, i: usize) {
        self.write(&i.to_ne_bytes());
    }

    fn write_i32(&mut self, i: i32) {
        self.write(&i.to_ne_bytes());
    }

    fn write_i64(&mut self, i: i64) {
        self.write(&i.to_ne_bytes());
    }

    fn write_isize(&mut self, i: isize) {
        self.write(&i.to_ne_bytes());
    }
}

pub type FastBuildHasher = BuildHasherDefault<FastHasher>;
pub type FastHashMap<K, V> = HashMap<K, V, FastBuildHasher>;
pub type FastHashSet<T> = HashSet<T, FastBuildHasher>;

pub fn arm64_code_cache_profile_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        crate::debug_env_var_os!("RUZU_PROFILE_ARM64_CODE_CACHE")
            .is_some_and(|value| value != std::ffi::OsStr::new("0"))
    })
}

#[cfg(test)]
mod tests {
    use super::FastBuildHasher;
    use std::hash::{BuildHasher, Hash, Hasher};

    fn hash<T: Hash>(value: T) -> u64 {
        let mut hasher = FastBuildHasher::default().build_hasher();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn composite_keys_mix_all_writes() {
        assert_ne!(hash((1u64, 2u64)), hash((3u64, 2u64)));
        assert_ne!(hash((1u64, 2u64)), hash((1u64, 3u64)));
    }
}
