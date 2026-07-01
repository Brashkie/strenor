//! Strenor native core.
//!
//! Design rule: this core NEVER interprets values. It stores opaque bytes
//! (`Vec<u8>`) keyed by string. The first byte of every value is a "tag" that
//! the JavaScript layer uses to know how to decode it — but Rust does not look
//! at it. The core only knows: bytes, keys, and expirations.
//!
//! That keeps the engine tiny and the value format fully owned by the JS layer,
//! which is exactly the architecture we want.

use napi::bindgen_prelude::Buffer;
use napi::{Error, Result, Status};
use napi_derive::napi;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Snapshot magic + format version. Bump VERSION on any breaking layout change.
const MAGIC: &[u8; 4] = b"STRN";
const VERSION: u8 = 1;

struct Entry {
    /// Opaque value bytes (includes the JS-side tag as byte 0). Never inspected here.
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

#[napi]
pub struct Strenor {
    inner: Mutex<HashMap<String, Entry>>,
}

#[napi]
impl Strenor {
    #[napi(constructor)]
    pub fn new() -> Self {
        Strenor {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Store raw bytes under `key`. `ttl_ms` (optional) sets an expiration in
    /// milliseconds from now. Writing a key always resets its TTL (Redis-like).
    #[napi]
    pub fn set(&self, key: String, value: Buffer, ttl_ms: Option<i64>) {
        let expire_at = match ttl_ms {
            Some(ms) if ms > 0 => Some(now_ms() + ms as u64),
            _ => None,
        };
        let mut map = self.inner.lock().unwrap();
        map.insert(
            key,
            Entry {
                value: value.to_vec(),
                expire_at,
            },
        );
    }

    /// Return the raw bytes for `key`, or `null` if missing or expired.
    /// Expiration is lazy: an expired key is removed on access.
    #[napi]
    pub fn get(&self, key: String) -> Option<Buffer> {
        let now = now_ms();
        let mut map = self.inner.lock().unwrap();
        match map.get(&key) {
            Some(e) if is_expired(e, now) => {
                map.remove(&key);
                None
            }
            Some(e) => Some(Buffer::from(e.value.clone())),
            None => None,
        }
    }

    /// Delete `key`. Returns true if it existed.
    #[napi]
    pub fn del(&self, key: String) -> bool {
        self.inner.lock().unwrap().remove(&key).is_some()
    }

    /// Whether `key` exists and is not expired.
    #[napi]
    pub fn exists(&self, key: String) -> bool {
        let now = now_ms();
        let mut map = self.inner.lock().unwrap();
        match map.get(&key) {
            Some(e) if is_expired(e, now) => {
                map.remove(&key);
                false
            }
            Some(_) => true,
            None => false,
        }
    }

    /// Set/replace the TTL (ms from now) of an existing key. Returns false if missing.
    #[napi]
    pub fn expire(&self, key: String, ttl_ms: i64) -> bool {
        let mut map = self.inner.lock().unwrap();
        match map.get_mut(&key) {
            Some(e) => {
                e.expire_at = if ttl_ms > 0 {
                    Some(now_ms() + ttl_ms as u64)
                } else {
                    Some(now_ms())
                };
                true
            }
            None => false,
        }
    }

    /// Remove the TTL of a key (make it persistent). Returns false if missing.
    #[napi]
    pub fn persist(&self, key: String) -> bool {
        let mut map = self.inner.lock().unwrap();
        match map.get_mut(&key) {
            Some(e) => {
                e.expire_at = None;
                true
            }
            None => false,
        }
    }

    /// Remaining TTL in ms. -1 = no expiry, -2 = key missing.
    #[napi]
    pub fn ttl(&self, key: String) -> i64 {
        let now = now_ms();
        let mut map = self.inner.lock().unwrap();
        match map.get(&key) {
            Some(e) if is_expired(e, now) => {
                map.remove(&key);
                -2
            }
            Some(e) => match e.expire_at {
                Some(t) => (t - now) as i64,
                None => -1,
            },
            None => -2,
        }
    }

    /// All live keys. O(n) and clones — intended for debugging/small datasets.
    #[napi]
    pub fn keys(&self) -> Vec<String> {
        let now = now_ms();
        let map = self.inner.lock().unwrap();
        map.iter()
            .filter(|(_, e)| !is_expired(e, now))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Number of entries (including not-yet-purged expired ones).
    #[napi]
    pub fn size(&self) -> u32 {
        self.inner.lock().unwrap().len() as u32
    }

    /// Remove all entries.
    #[napi]
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }

    /// Eagerly purge expired entries. Returns how many were removed.
    #[napi]
    pub fn sweep(&self) -> u32 {
        let now = now_ms();
        let mut map = self.inner.lock().unwrap();
        let before = map.len();
        map.retain(|_, e| !is_expired(e, now));
        (before - map.len()) as u32
    }

    /// Dump the full store to a self-describing binary snapshot at `path`.
    ///
    /// Layout (little-endian):
    ///   "STRN" | version:u8 | flags:u8 | count:u32
    ///   per entry: key_len:u32 | key | expire_at:u64 (0 = none) | val_len:u32 | val
    #[napi]
    pub fn dump(&self, path: String) -> Result<()> {
        let map = self.inner.lock().unwrap();
        let mut buf: Vec<u8> = Vec::new();
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
        std::fs::write(&path, &buf)
            .map_err(|err| Error::new(Status::GenericFailure, format!("dump failed: {err}")))
    }

    /// Load a snapshot, replacing all current in-memory state.
    #[napi]
    pub fn load(&self, path: String) -> Result<()> {
        let data = std::fs::read(&path)
            .map_err(|err| Error::new(Status::GenericFailure, format!("load failed: {err}")))?;

        let bad = |m: &str| Error::new(Status::InvalidArg, m.to_string());

        let take = |buf: &[u8], p: &mut usize, n: usize| -> Result<()> {
            if *p + n > buf.len() {
                return Err(Error::new(Status::InvalidArg, "snapshot truncated"));
            }
            *p += n;
            Ok(())
        };

        if data.len() < 10 {
            return Err(bad("snapshot too small"));
        }
        if &data[0..4] != MAGIC {
            return Err(bad("bad magic: not a Strenor snapshot"));
        }
        let version = data[4];
        if version != VERSION {
            return Err(bad("unsupported snapshot version"));
        }
        let mut p: usize = 6; // skip magic(4) + version(1) + flags(1)

        let mut count_bytes = [0u8; 4];
        count_bytes.copy_from_slice(&data[p..p + 4]);
        p += 4;
        let count = u32::from_le_bytes(count_bytes) as usize;

        let mut next = HashMap::with_capacity(count);
        for _ in 0..count {
            // key
            take(&data, &mut p, 4)?;
            let kl = u32::from_le_bytes(data[p - 4..p].try_into().unwrap()) as usize;
            take(&data, &mut p, kl)?;
            let key = String::from_utf8(data[p - kl..p].to_vec())
                .map_err(|_| bad("invalid utf8 key"))?;
            // expire_at
            take(&data, &mut p, 8)?;
            let exp = u64::from_le_bytes(data[p - 8..p].try_into().unwrap());
            // value
            take(&data, &mut p, 4)?;
            let vl = u32::from_le_bytes(data[p - 4..p].try_into().unwrap()) as usize;
            take(&data, &mut p, vl)?;
            let value = data[p - vl..p].to_vec();

            next.insert(
                key,
                Entry {
                    value,
                    expire_at: if exp == 0 { None } else { Some(exp) },
                },
            );
        }

        *self.inner.lock().unwrap() = next;
        Ok(())
    }
}

impl Default for Strenor {
    fn default() -> Self {
        Self::new()
    }
}
