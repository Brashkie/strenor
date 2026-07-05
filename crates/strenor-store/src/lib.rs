//! Strenor's pure key-value core.
//!
//! Two layers of "type" are kept strictly separate:
//! - **This engine** knows only *structure* types (`Bytes`, `List`, and — later —
//!   `Hash`, `Set`, `SortedSet`). It never interprets the content of a blob.
//! - The **JavaScript layer** owns *content* tags (raw/string/json/codec) via the
//!   first byte of each blob.
//!
//! So every stored blob — a plain value or a single list element — is the same
//! opaque `[tag][payload]` that `set()` uses. The engine just moves those blobs
//! around; it never runs a `JSON.parse`.

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

/// Snapshot magic. `VERSION` 2 adds a per-entry structure-type byte (lists);
/// version 1 snapshots (bytes only) are still readable.
pub const MAGIC: &[u8; 4] = b"STRN";
pub const VERSION: u8 = 2;

const TYPE_BYTES: u8 = 0;
const TYPE_LIST: u8 = 1;

/// A structure-typed value. The engine distinguishes these; it never looks
/// inside a blob. New engine structures (Hash, Set, SortedSet) will be added
/// here in later versions.
enum Value {
    Bytes(Vec<u8>),
    List(VecDeque<Vec<u8>>),
}

/// A stored record: a typed value plus an optional expiration (TTL applies to
/// any structure type, exactly like Redis).
struct Entry {
    value: Value,
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

/// Operating on a key that holds a different structure type (Redis `WRONGTYPE`).
#[derive(Debug, PartialEq, Eq)]
pub struct WrongType;

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

    fn expire_at_from_ttl(ttl_ms: Option<i64>) -> Option<u64> {
        match ttl_ms {
            Some(ms) if ms > 0 => Some(now_ms() + ms as u64),
            _ => None,
        }
    }

    // ── Bytes (KV) ────────────────────────────────────────────────────────

    /// Store raw bytes. Replaces any existing value (including a list), like SET.
    pub fn set(&self, key: String, value: Vec<u8>, ttl_ms: Option<i64>) {
        let expire_at = Self::expire_at_from_ttl(ttl_ms);
        self.inner.lock().insert(
            key,
            Entry {
                value: Value::Bytes(value),
                expire_at,
            },
        );
    }

    /// Return the bytes for `key`. `WrongType` if it holds a list.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, WrongType> {
        let now = now_ms();
        let mut map = self.inner.lock();
        match map.get(key) {
            Some(e) if is_expired(e, now) => {
                map.remove(key);
                Ok(None)
            }
            Some(e) => match &e.value {
                Value::Bytes(bytes) => Ok(Some(bytes.clone())),
                Value::List(_) => Err(WrongType),
            },
            None => Ok(None),
        }
    }

    // ── Key management (type-agnostic) ────────────────────────────────────

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

    pub fn keys(&self) -> Vec<String> {
        let now = now_ms();
        self.inner
            .lock()
            .iter()
            .filter(|(_, e)| !is_expired(e, now))
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub fn size(&self) -> u32 {
        self.inner.lock().len() as u32
    }

    pub fn clear(&self) {
        self.inner.lock().clear();
    }

    pub fn sweep(&self) -> u32 {
        let now = now_ms();
        let mut map = self.inner.lock();
        let before = map.len();
        map.retain(|_, e| !is_expired(e, now));
        (before - map.len()) as u32
    }

    // ── List ──────────────────────────────────────────────────────────────

    fn push(&self, key: &str, value: Vec<u8>, front: bool) -> Result<u32, WrongType> {
        let now = now_ms();
        let mut map = self.inner.lock();
        if map.get(key).map(|e| is_expired(e, now)).unwrap_or(false) {
            map.remove(key);
        }
        match map.get_mut(key) {
            Some(e) => match &mut e.value {
                Value::List(l) => {
                    if front {
                        l.push_front(value);
                    } else {
                        l.push_back(value);
                    }
                    Ok(l.len() as u32)
                }
                Value::Bytes(_) => Err(WrongType),
            },
            None => {
                let mut l = VecDeque::new();
                l.push_back(value);
                map.insert(
                    key.to_string(),
                    Entry {
                        value: Value::List(l),
                        expire_at: None,
                    },
                );
                Ok(1)
            }
        }
    }

    /// Prepend to a list (creating it if missing). Returns the new length.
    pub fn push_front(&self, key: &str, value: Vec<u8>) -> Result<u32, WrongType> {
        self.push(key, value, true)
    }

    /// Append to a list (creating it if missing). Returns the new length.
    pub fn push_back(&self, key: &str, value: Vec<u8>) -> Result<u32, WrongType> {
        self.push(key, value, false)
    }

    fn pop(&self, key: &str, front: bool) -> Result<Option<Vec<u8>>, WrongType> {
        let now = now_ms();
        let mut map = self.inner.lock();
        if map.get(key).map(|e| is_expired(e, now)).unwrap_or(false) {
            map.remove(key);
            return Ok(None);
        }
        match map.get_mut(key) {
            Some(e) => match &mut e.value {
                Value::List(l) => {
                    let v = if front { l.pop_front() } else { l.pop_back() };
                    if l.is_empty() {
                        map.remove(key); // an empty list key is deleted (Redis-like)
                    }
                    Ok(v)
                }
                Value::Bytes(_) => Err(WrongType),
            },
            None => Ok(None),
        }
    }

    /// Remove and return the first element, or `None` if empty/missing.
    pub fn pop_front(&self, key: &str) -> Result<Option<Vec<u8>>, WrongType> {
        self.pop(key, true)
    }

    /// Remove and return the last element, or `None` if empty/missing.
    pub fn pop_back(&self, key: &str) -> Result<Option<Vec<u8>>, WrongType> {
        self.pop(key, false)
    }

    /// List length (0 if missing). `WrongType` if the key holds bytes.
    pub fn llen(&self, key: &str) -> Result<u32, WrongType> {
        let now = now_ms();
        let mut map = self.inner.lock();
        if map.get(key).map(|e| is_expired(e, now)).unwrap_or(false) {
            map.remove(key);
            return Ok(0);
        }
        match map.get(key) {
            Some(e) => match &e.value {
                Value::List(l) => Ok(l.len() as u32),
                Value::Bytes(_) => Err(WrongType),
            },
            None => Ok(0),
        }
    }

    /// Elements in the inclusive range `[start, stop]`, Redis-style (negative
    /// indices count from the end; out-of-range is clamped).
    pub fn lrange(&self, key: &str, start: i64, stop: i64) -> Result<Vec<Vec<u8>>, WrongType> {
        let now = now_ms();
        let mut map = self.inner.lock();
        if map.get(key).map(|e| is_expired(e, now)).unwrap_or(false) {
            map.remove(key);
            return Ok(Vec::new());
        }
        match map.get(key) {
            Some(e) => match &e.value {
                Value::List(l) => {
                    let len = l.len() as i64;
                    let mut s = if start < 0 { len + start } else { start };
                    let mut t = if stop < 0 { len + stop } else { stop };
                    if s < 0 {
                        s = 0;
                    }
                    if t >= len {
                        t = len - 1;
                    }
                    if len == 0 || s > t || s >= len {
                        return Ok(Vec::new());
                    }
                    Ok(l.iter()
                        .skip(s as usize)
                        .take((t - s + 1) as usize)
                        .cloned()
                        .collect())
                }
                Value::Bytes(_) => Err(WrongType),
            },
            None => Ok(Vec::new()),
        }
    }

    // ── Snapshot ──────────────────────────────────────────────────────────

    /// Serialize the whole store to a self-describing binary snapshot.
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
            match &e.value {
                Value::Bytes(bytes) => {
                    buf.push(TYPE_BYTES);
                    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                    buf.extend_from_slice(bytes);
                }
                Value::List(l) => {
                    buf.push(TYPE_LIST);
                    buf.extend_from_slice(&(l.len() as u32).to_le_bytes());
                    for elem in l {
                        buf.extend_from_slice(&(elem.len() as u32).to_le_bytes());
                        buf.extend_from_slice(elem);
                    }
                }
            }
        }
        buf
    }

    /// Load a snapshot, replacing all current state. Accepts version 1 and 2.
    pub fn load_bytes(&self, data: &[u8]) -> Result<(), SnapshotError> {
        let bad = |m: &str| SnapshotError(m.to_string());
        if data.len() < 10 {
            return Err(bad("snapshot too small"));
        }
        if &data[0..4] != MAGIC {
            return Err(bad("bad magic: not a Strenor snapshot"));
        }
        let version = data[4];
        if version != 1 && version != 2 {
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
            let expire_at = if exp == 0 { None } else { Some(exp) };

            // Version 1: bytes only, no type byte. Version 2: type byte first.
            let kind = if version == 1 {
                TYPE_BYTES
            } else {
                let at = read(&mut p, 1)?;
                data[at]
            };

            let value = match kind {
                TYPE_BYTES => {
                    let at = read(&mut p, 4)?;
                    let vl = u32_at(data, at) as usize;
                    let at = read(&mut p, vl)?;
                    Value::Bytes(data[at..at + vl].to_vec())
                }
                TYPE_LIST => {
                    let at = read(&mut p, 4)?;
                    let n = u32_at(data, at) as usize;
                    let mut l = VecDeque::with_capacity(n);
                    for _ in 0..n {
                        let at = read(&mut p, 4)?;
                        let el = u32_at(data, at) as usize;
                        let at = read(&mut p, el)?;
                        l.push_back(data[at..at + el].to_vec());
                    }
                    Value::List(l)
                }
                _ => return Err(bad("unknown entry type in snapshot")),
            };

            next.insert(key, Entry { value, expire_at });
        }

        *self.inner.lock() = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    #[test]
    fn set_get_del() {
        let s = Store::new();
        s.set("a".into(), vec![1, 2, 3], None);
        assert_eq!(s.get("a"), Ok(Some(vec![1, 2, 3])));
        assert!(s.exists("a"));
        assert!(s.del("a"));
        assert!(!s.del("a"));
        assert_eq!(s.get("a"), Ok(None));
    }

    #[test]
    fn ttl_semantics() {
        let s = Store::new();
        s.set("k".into(), vec![0], None);
        assert_eq!(s.ttl("k"), -1);
        assert_eq!(s.ttl("ghost"), -2);
        assert!(s.expire("k", 10_000));
        assert!(s.ttl("k") > 0);
        assert!(s.persist("k"));
        assert_eq!(s.ttl("k"), -1);
        assert!(!s.expire("missing", 1000));
    }

    #[test]
    fn expiration_and_sweep() {
        let s = Store::new();
        s.set("x".into(), vec![0], Some(1));
        s.set("y".into(), vec![0], None);
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(s.get("x"), Ok(None));
        s.set("z".into(), vec![0], Some(1));
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(s.sweep(), 1);
        assert!(s.exists("y"));
    }

    #[test]
    fn list_fifo_and_lifo() {
        let s = Store::new();
        assert_eq!(s.push_back("q", b("a")), Ok(1));
        assert_eq!(s.push_back("q", b("b")), Ok(2));
        assert_eq!(s.pop_front("q"), Ok(Some(b("a"))));
        assert_eq!(s.pop_front("q"), Ok(Some(b("b"))));
        assert_eq!(s.pop_front("q"), Ok(None));
        assert!(!s.exists("q"));

        s.push_back("s", b("1")).unwrap();
        s.push_back("s", b("2")).unwrap();
        assert_eq!(s.pop_back("s"), Ok(Some(b("2"))));
        assert_eq!(s.pop_back("s"), Ok(Some(b("1"))));
    }

    #[test]
    fn llen_and_lrange() {
        let s = Store::new();
        for c in ["a", "b", "c", "d"] {
            s.push_back("l", b(c)).unwrap();
        }
        assert_eq!(s.llen("l"), Ok(4));
        assert_eq!(
            s.lrange("l", 0, -1),
            Ok(vec![b("a"), b("b"), b("c"), b("d")])
        );
        assert_eq!(s.lrange("l", 1, 2), Ok(vec![b("b"), b("c")]));
        assert_eq!(s.lrange("l", -2, -1), Ok(vec![b("c"), b("d")]));
        assert_eq!(s.lrange("l", 10, 20), Ok(Vec::new()));
        assert_eq!(s.lrange("missing", 0, -1), Ok(Vec::new()));
        assert_eq!(s.llen("missing"), Ok(0));
    }

    #[test]
    fn wrong_type_errors() {
        let s = Store::new();
        s.set("a".into(), b("hola"), None);
        assert_eq!(s.push_back("a", b("x")), Err(WrongType));
        assert_eq!(s.pop_front("a"), Err(WrongType));
        assert_eq!(s.llen("a"), Err(WrongType));
        assert_eq!(s.lrange("a", 0, -1), Err(WrongType));

        s.push_back("l", b("x")).unwrap();
        assert_eq!(s.get("l"), Err(WrongType));
        s.set("l".into(), b("now bytes"), None);
        assert_eq!(s.get("l"), Ok(Some(b("now bytes"))));
    }

    #[test]
    fn snapshot_roundtrip_bytes_and_lists() {
        let s = Store::new();
        s.set("name".into(), b("strenor"), None);
        s.set("ttl".into(), vec![1], Some(60_000));
        s.push_back("jobs", b("j1")).unwrap();
        s.push_back("jobs", b("j2")).unwrap();
        let bytes = s.dump_bytes();

        let fresh = Store::new();
        fresh.load_bytes(&bytes).unwrap();
        assert_eq!(fresh.get("name"), Ok(Some(b("strenor"))));
        assert!(fresh.ttl("ttl") > 0);
        assert_eq!(fresh.lrange("jobs", 0, -1), Ok(vec![b("j1"), b("j2")]));
    }

    #[test]
    fn snapshot_rejects_garbage() {
        let s = Store::new();
        assert!(s.load_bytes(b"not a snapshot").is_err());
        assert!(s.load_bytes(b"STRN\x09\x00\x00\x00\x00\x00").is_err());
    }
}
