//! Strenor's pure key-value core: an in-memory, value-agnostic byte store with
//! TTL and a self-describing binary snapshot. No FFI here — this crate knows
//! nothing about Node or NAPI, which keeps it small and fully unit-testable.
//!
//! The store never interprets values: it holds opaque `Vec<u8>` keyed by string.
//! The first byte of each value is a "tag" owned by the JavaScript layer; this
//! core does not look at it.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Snapshot magic + format version. Bump `VERSION` on any breaking layout change.
pub const MAGIC: &[u8; 4] = b"STRN";
pub const VERSION: u8 = 1;

struct Entry {
    /// Opaque value bytes (includes the JS-side tag as byte 0). Never inspected.
    value: Vec<u8>,
    /// Absolute expiration in ms since epoch. `None` = never expires.
    expire_at: Option<u64>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn is_expired(e: &Entry, now: u64) -> bool {
    matches!(e.expire_at, Some(t) if t <= now)
}

/// Error returned when a snapshot cannot be parsed.
#[derive(Debug, PartialEq, Eq)]
pub struct SnapshotError(pub String);

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// In-memory key-value store. `parking_lot::Mutex` gives fast, poison-free locking.
#[derive(Default)]
pub struct Store {
    inner: Mutex<HashMap<String, Entry>>,
}

impl Store {
    pub fn new() -> Self {
        Store {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Store raw bytes. `ttl_ms > 0` sets an expiration; writing always resets TTL.
    pub fn set(&self, key: String, value: Vec<u8>, ttl_ms: Option<i64>) {
        let expire_at = match ttl_ms {
            Some(ms) if ms > 0 => Some(now_ms() + ms as u64),
            _ => None,
        };
        self.inner.lock().insert(key, Entry { value, expire_at });
    }

    /// Return the bytes for `key`, or `None` if missing/expired (lazy removal).
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        let now = now_ms();
        let mut map = self.inner.lock();
        match map.get(key) {
            Some(e) if is_expired(e, now) => {
                map.remove(key);
                None
            }
            Some(e) => Some(e.value.clone()),
            None => None,
        }
    }

    pub fn del(&self, key: &str) -> bool {
        self.inner.lock().remove(key).is_some()
    }

    pub fn exists(&self, key: &str) -> bool {
        let now = now_ms();
        let mut map = self.inner.lock();
        match map.get(key) {
            Some(e) if is_expired(e, now) => {
                map.remove(key);
                false
            }
            Some(_) => true,
            None => false,
        }
    }

    /// Set/replace the TTL (ms from now) of an existing key. False if missing.
    pub fn expire(&self, key: &str, ttl_ms: i64) -> bool {
        let mut map = self.inner.lock();
        match map.get_mut(key) {
            Some(e) => {
                e.expire_at = Some(now_ms() + ttl_ms.max(0) as u64);
                true
            }
            None => false,
        }
    }

    /// Remove a key's TTL (make it persistent). False if missing.
    pub fn persist(&self, key: &str) -> bool {
        let mut map = self.inner.lock();
        match map.get_mut(key) {
            Some(e) => {
                e.expire_at = None;
                true
            }
            None => false,
        }
    }

    /// Remaining TTL in ms. `-1` = no expiry, `-2` = missing.
    pub fn ttl(&self, key: &str) -> i64 {
        let now = now_ms();
        let mut map = self.inner.lock();
        match map.get(key) {
            Some(e) if is_expired(e, now) => {
                map.remove(key);
                -2
            }
            Some(e) => match e.expire_at {
                Some(t) => (t - now) as i64,
                None => -1,
            },
            None => -2,
        }
    }

    /// All live keys. O(n); intended for debugging/small datasets.
    pub fn keys(&self) -> Vec<String> {
        let now = now_ms();
        self.inner
            .lock()
            .iter()
            .filter(|(_, e)| !is_expired(e, now))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Entry count (including expired entries not yet swept — Redis-like lazy model).
    pub fn size(&self) -> u32 {
        self.inner.lock().len() as u32
    }

    pub fn clear(&self) {
        self.inner.lock().clear();
    }

    /// Eagerly purge expired entries. Returns how many were removed.
    pub fn sweep(&self) -> u32 {
        let now = now_ms();
        let mut map = self.inner.lock();
        let before = map.len();
        map.retain(|_, e| !is_expired(e, now));
        (before - map.len()) as u32
    }

    /// Serialize the whole store to a self-describing binary snapshot.
    ///
    /// Layout (little-endian):
    ///   "STRN" | version:u8 | flags:u8 | count:u32
    ///   per entry: key_len:u32 | key | expire_at:u64 (0 = none) | val_len:u32 | val
    pub fn dump_bytes(&self) -> Vec<u8> {
        let map = self.inner.lock();
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.push(0u8); // flags (reserved)
        buf.extend_from_slice(&(map.len() as u32).to_le_bytes());
        for (k, e) in map.iter() {
            let kb = k.as_bytes();
            buf.extend_from_slice(&(kb.len() as u32).to_le_bytes());
            buf.extend_from_slice(kb);
            buf.extend_from_slice(&e.expire_at.unwrap_or(0).to_le_bytes());
            buf.extend_from_slice(&(e.value.len() as u32).to_le_bytes());
            buf.extend_from_slice(&e.value);
        }
        buf
    }

    /// Load a snapshot, replacing all current state.
    pub fn load_bytes(&self, data: &[u8]) -> Result<(), SnapshotError> {
        let bad = |m: &str| SnapshotError(m.to_string());
        if data.len() < 10 {
            return Err(bad("snapshot too small"));
        }
        if &data[0..4] != MAGIC {
            return Err(bad("bad magic: not a Strenor snapshot"));
        }
        if data[4] != VERSION {
            return Err(bad("unsupported snapshot version"));
        }
        let mut p = 6usize; // magic(4) + version(1) + flags(1)

        let read = |p: &mut usize, n: usize| -> Result<usize, SnapshotError> {
            if *p + n > data.len() {
                return Err(SnapshotError("snapshot truncated".into()));
            }
            let start = *p;
            *p += n;
            Ok(start)
        };
        let u32_at =
            |data: &[u8], at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        let u64_at =
            |data: &[u8], at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());

        let at = read(&mut p, 4)?;
        let count = u32_at(data, at) as usize;

        let mut next = HashMap::with_capacity(count);
        for _ in 0..count {
            let at = read(&mut p, 4)?;
            let kl = u32_at(data, at) as usize;
            let at = read(&mut p, kl)?;
            let key = String::from_utf8(data[at..at + kl].to_vec())
                .map_err(|_| bad("invalid utf8 key"))?;

            let at = read(&mut p, 8)?;
            let exp = u64_at(data, at);

            let at = read(&mut p, 4)?;
            let vl = u32_at(data, at) as usize;
            let at = read(&mut p, vl)?;
            let value = data[at..at + vl].to_vec();

            next.insert(
                key,
                Entry {
                    value,
                    expire_at: if exp == 0 { None } else { Some(exp) },
                },
            );
        }

        *self.inner.lock() = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_del() {
        let s = Store::new();
        s.set("a".into(), vec![1, 2, 3], None);
        assert_eq!(s.get("a"), Some(vec![1, 2, 3]));
        assert!(s.exists("a"));
        assert!(s.del("a"));
        assert!(!s.del("a"));
        assert_eq!(s.get("a"), None);
    }

    #[test]
    fn size_clear_keys() {
        let s = Store::new();
        s.set("a".into(), vec![0], None);
        s.set("b".into(), vec![0], None);
        assert_eq!(s.size(), 2);
        let mut ks = s.keys();
        ks.sort();
        assert_eq!(ks, vec!["a".to_string(), "b".to_string()]);
        s.clear();
        assert_eq!(s.size(), 0);
    }

    #[test]
    fn ttl_semantics() {
        let s = Store::new();
        s.set("k".into(), vec![0], None);
        assert_eq!(s.ttl("k"), -1); // no expiry
        assert_eq!(s.ttl("ghost"), -2); // missing
        assert!(s.expire("k", 10_000));
        assert!(s.ttl("k") > 0);
        assert!(s.persist("k"));
        assert_eq!(s.ttl("k"), -1);
        assert!(!s.expire("missing", 1000));
    }

    #[test]
    fn expiration_and_sweep() {
        let s = Store::new();
        s.set("x".into(), vec![0], Some(1)); // 1ms TTL
        s.set("y".into(), vec![0], None);
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(s.get("x"), None); // lazily expired
        s.set("z".into(), vec![0], Some(1));
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(s.sweep(), 1); // z purged
        assert!(s.exists("y"));
    }

    #[test]
    fn snapshot_roundtrip() {
        let s = Store::new();
        s.set("keep".into(), vec![9, 8, 7], None);
        s.set("name".into(), b"strenor".to_vec(), None);
        let bytes = s.dump_bytes();

        let fresh = Store::new();
        fresh.load_bytes(&bytes).unwrap();
        assert_eq!(fresh.get("keep"), Some(vec![9, 8, 7]));
        assert_eq!(fresh.get("name"), Some(b"strenor".to_vec()));
    }

    #[test]
    fn snapshot_rejects_garbage() {
        let s = Store::new();
        assert!(s.load_bytes(b"not a snapshot").is_err());
        assert!(s.load_bytes(b"STRN\x02").is_err()); // wrong version / too small
    }

    #[test]
    fn snapshot_preserves_ttl() {
        let s = Store::new();
        s.set("t".into(), vec![1], Some(60_000));
        let fresh = Store::new();
        fresh.load_bytes(&s.dump_bytes()).unwrap();
        assert!(fresh.ttl("t") > 0);
    }
}
