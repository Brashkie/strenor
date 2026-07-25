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
//!
//! Durability is optional: with an append-only log attached (`with_aof`), every
//! mutation is journalled and replayed on startup.

mod aof;

use aof::Aof;
pub use aof::{crc32, write_atomic};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Snapshot magic and current format version.
/// - v1: bytes only.
/// - v2: adds a structure-type byte per entry (lists).
/// - v3: adds a trailing CRC-32 over the body (corruption detection).
/// - v4: adds the hash structure type.
///
/// Older versions still load, so existing snapshots keep working.
pub const MAGIC: &[u8; 4] = b"STRN";
pub const VERSION: u8 = 4;

const TYPE_BYTES: u8 = 0;
const TYPE_LIST: u8 = 1;
const TYPE_HASH: u8 = 2;

/// A structure-typed value. The engine distinguishes these; it never looks
/// inside a blob. New engine structures (Hash, Set, SortedSet) will be added
/// here in later versions.
enum Value {
    Bytes(Vec<u8>),
    List(VecDeque<Vec<u8>>),
    Hash(HashMap<String, Vec<u8>>),
}

/// A stored record: a typed value plus an optional expiration (TTL applies to
/// any structure type, exactly like Redis).
struct Entry {
    value: Value,
    expire_at: Option<u64>,
}

/// State + log behind a single lock, so the log order always matches the order
/// mutations were applied. Journalling outside the lock would let two threads
/// interleave and replay into a different state than the one in memory.
struct Inner {
    map: HashMap<String, Entry>,
    aof: Option<Aof>,
    closed: bool,
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

/// What replaying a log on startup found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recovery {
    /// Records successfully applied.
    pub applied: u32,
    /// True when a torn/corrupt tail was found and dropped (a crash mid-append).
    pub truncated: bool,
}

// ── Mutations, applied to the map only (no journalling) ───────────────────
// Used by the public API *and* by replay, so a replayed log reproduces exactly
// the same state — there is only one implementation of each operation.

fn apply_set(
    map: &mut HashMap<String, Entry>,
    key: String,
    value: Vec<u8>,
    expire_at: Option<u64>,
) {
    map.insert(
        key,
        Entry {
            value: Value::Bytes(value),
            expire_at,
        },
    );
}

fn apply_del(map: &mut HashMap<String, Entry>, key: &str) -> bool {
    map.remove(key).is_some()
}

fn apply_expire(map: &mut HashMap<String, Entry>, key: &str, expire_at: u64) -> bool {
    match map.get_mut(key) {
        Some(e) => {
            e.expire_at = Some(expire_at);
            true
        }
        None => false,
    }
}

fn apply_persist(map: &mut HashMap<String, Entry>, key: &str) -> bool {
    match map.get_mut(key) {
        Some(e) => {
            e.expire_at = None;
            true
        }
        None => false,
    }
}

fn apply_push(
    map: &mut HashMap<String, Entry>,
    key: &str,
    value: Vec<u8>,
    front: bool,
) -> Result<u32, WrongType> {
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
            _ => Err(WrongType),
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

fn apply_hset(
    map: &mut HashMap<String, Entry>,
    key: &str,
    field: String,
    value: Vec<u8>,
) -> Result<bool, WrongType> {
    match map.get_mut(key) {
        Some(e) => match &mut e.value {
            Value::Hash(h) => Ok(h.insert(field, value).is_none()),
            _ => Err(WrongType),
        },
        None => {
            let mut h = HashMap::new();
            h.insert(field, value);
            map.insert(
                key.to_string(),
                Entry {
                    value: Value::Hash(h),
                    expire_at: None,
                },
            );
            Ok(true)
        }
    }
}

fn apply_hdel(map: &mut HashMap<String, Entry>, key: &str, field: &str) -> Result<bool, WrongType> {
    match map.get_mut(key) {
        Some(e) => match &mut e.value {
            Value::Hash(h) => {
                let existed = h.remove(field).is_some();
                if h.is_empty() {
                    map.remove(key); // an empty hash key is deleted (Redis-like)
                }
                Ok(existed)
            }
            _ => Err(WrongType),
        },
        None => Ok(false),
    }
}

fn apply_pop(
    map: &mut HashMap<String, Entry>,
    key: &str,
    front: bool,
) -> Result<Option<Vec<u8>>, WrongType> {
    match map.get_mut(key) {
        Some(e) => match &mut e.value {
            Value::List(l) => {
                let v = if front { l.pop_front() } else { l.pop_back() };
                if l.is_empty() {
                    map.remove(key); // an empty list key is deleted (Redis-like)
                }
                Ok(v)
            }
            _ => Err(WrongType),
        },
        None => Ok(None),
    }
}

/// In-memory store. `parking_lot::Mutex` gives fast, poison-free locking.
pub struct Store {
    inner: Mutex<Inner>,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    pub fn new() -> Self {
        Store {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                aof: None,
                closed: false,
            }),
        }
    }

    /// Open a store backed by an append-only log at `path`, replaying it first.
    ///
    /// A torn tail (the process died mid-append) is dropped and the file
    /// truncated to the last intact record — reported via [`Recovery`], not an
    /// error, because it is the expected outcome of a crash.
    pub fn with_aof(path: &Path, fsync: bool) -> std::io::Result<(Self, Recovery)> {
        let (records, damaged_at) = aof::read_records(path)?;
        let mut map = HashMap::new();
        let mut applied = 0u32;

        for payload in &records {
            if Self::replay_one(&mut map, payload) {
                applied += 1;
            }
        }
        if let Some(offset) = damaged_at {
            aof::truncate_at(path, offset)?;
        }

        let store = Store {
            inner: Mutex::new(Inner {
                map,
                aof: Some(Aof::open(path, fsync)?),
                closed: false,
            }),
        };
        Ok((
            store,
            Recovery {
                applied,
                truncated: damaged_at.is_some(),
            },
        ))
    }

    /// Apply one decoded log record. Returns false if the record is unreadable.
    fn replay_one(map: &mut HashMap<String, Entry>, payload: &[u8]) -> bool {
        if payload.is_empty() {
            return false;
        }
        let op = payload[0];
        let mut c = aof::Cursor::new(payload);
        match op {
            aof::OP_SET => match (c.string(), c.u64(), c.bytes()) {
                (Some(k), Some(exp), Some(v)) => {
                    apply_set(map, k, v.to_vec(), if exp == 0 { None } else { Some(exp) });
                    true
                }
                _ => false,
            },
            aof::OP_DEL => match c.string() {
                Some(k) => {
                    apply_del(map, &k);
                    true
                }
                None => false,
            },
            aof::OP_EXPIRE => match (c.string(), c.u64()) {
                (Some(k), Some(exp)) => {
                    apply_expire(map, &k, exp);
                    true
                }
                _ => false,
            },
            aof::OP_PERSIST => match c.string() {
                Some(k) => {
                    apply_persist(map, &k);
                    true
                }
                None => false,
            },
            aof::OP_CLEAR => {
                map.clear();
                true
            }
            aof::OP_PUSH_FRONT | aof::OP_PUSH_BACK => match (c.string(), c.bytes()) {
                (Some(k), Some(v)) => {
                    let _ = apply_push(map, &k, v.to_vec(), op == aof::OP_PUSH_FRONT);
                    true
                }
                _ => false,
            },
            aof::OP_POP_FRONT | aof::OP_POP_BACK => match c.string() {
                Some(k) => {
                    let _ = apply_pop(map, &k, op == aof::OP_POP_FRONT);
                    true
                }
                None => false,
            },
            aof::OP_HSET => match (c.string(), c.string(), c.bytes()) {
                (Some(k), Some(field), Some(v)) => {
                    let _ = apply_hset(map, &k, field, v.to_vec());
                    true
                }
                _ => false,
            },
            aof::OP_HDEL => match (c.string(), c.string()) {
                (Some(k), Some(field)) => {
                    let _ = apply_hdel(map, &k, &field);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Journal a record if a log is attached. Errors are surfaced: a write that
    /// isn't durable must not be reported as successful.
    fn journal(inner: &mut Inner, payload: Vec<u8>) -> std::io::Result<()> {
        match inner.aof.as_mut() {
            Some(a) => a.append(&payload),
            None => Ok(()),
        }
    }

    /// Reject mutations on a closed store. Silently accepting them would drop
    /// writes that were never journalled — data loss with no error.
    fn ensure_open(inner: &Inner) -> std::io::Result<()> {
        if inner.closed {
            return Err(std::io::Error::other("store is closed"));
        }
        Ok(())
    }

    /// Flush the log and release its file handle. Idempotent.
    ///
    /// Releasing the handle matters beyond tidiness: on Windows a file with an
    /// open handle cannot be deleted or replaced, so without this the log stays
    /// locked for the life of the process. After closing, mutations fail rather
    /// than silently skipping the journal; reads still work from memory.
    pub fn close(&self) -> std::io::Result<()> {
        let mut inner = self.inner.lock();
        inner.closed = true;
        match inner.aof.take() {
            Some(mut a) => a.close(),
            None => Ok(()),
        }
    }

    /// Whether `close` has been called.
    pub fn is_closed(&self) -> bool {
        self.inner.lock().closed
    }

    fn expire_at_from_ttl(ttl_ms: Option<i64>) -> Option<u64> {
        match ttl_ms {
            Some(ms) if ms > 0 => Some(now_ms() + ms as u64),
            _ => None,
        }
    }

    // ── Bytes (KV) ────────────────────────────────────────────────────────

    /// Store raw bytes. Replaces any existing value (including a list), like SET.
    pub fn set(&self, key: String, value: Vec<u8>, ttl_ms: Option<i64>) -> std::io::Result<()> {
        let expire_at = Self::expire_at_from_ttl(ttl_ms);
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        let rec = aof::rec_set(&key, &value, expire_at);
        apply_set(&mut inner.map, key, value, expire_at);
        Self::journal(&mut inner, rec)
    }

    /// Return the bytes for `key`. `WrongType` if it holds a list.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        match inner.map.get(key) {
            Some(e) if is_expired(e, now) => {
                inner.map.remove(key);
                Ok(None)
            }
            Some(e) => match &e.value {
                Value::Bytes(bytes) => Ok(Some(bytes.clone())),
                _ => Err(WrongType),
            },
            None => Ok(None),
        }
    }

    // ── Key management (type-agnostic) ────────────────────────────────────

    pub fn del(&self, key: &str) -> std::io::Result<bool> {
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        let existed = apply_del(&mut inner.map, key);
        if existed {
            Self::journal(&mut inner, aof::rec_key_only(aof::OP_DEL, key))?;
        }
        Ok(existed)
    }

    pub fn exists(&self, key: &str) -> bool {
        let now = now_ms();
        let mut inner = self.inner.lock();
        match inner.map.get(key) {
            Some(e) if is_expired(e, now) => {
                inner.map.remove(key);
                false
            }
            Some(_) => true,
            None => false,
        }
    }

    pub fn expire(&self, key: &str, ttl_ms: i64) -> std::io::Result<bool> {
        let expire_at = now_ms() + ttl_ms.max(0) as u64;
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        let ok = apply_expire(&mut inner.map, key, expire_at);
        if ok {
            Self::journal(&mut inner, aof::rec_expire(key, expire_at))?;
        }
        Ok(ok)
    }

    pub fn persist(&self, key: &str) -> std::io::Result<bool> {
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        let ok = apply_persist(&mut inner.map, key);
        if ok {
            Self::journal(&mut inner, aof::rec_key_only(aof::OP_PERSIST, key))?;
        }
        Ok(ok)
    }

    pub fn ttl(&self, key: &str) -> i64 {
        let now = now_ms();
        let mut inner = self.inner.lock();
        match inner.map.get(key) {
            Some(e) if is_expired(e, now) => {
                inner.map.remove(key);
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
            .map
            .iter()
            .filter(|(_, e)| !is_expired(e, now))
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub fn size(&self) -> u32 {
        self.inner.lock().map.len() as u32
    }

    pub fn clear(&self) -> std::io::Result<()> {
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        inner.map.clear();
        Self::journal(&mut inner, aof::rec_clear())
    }

    pub fn sweep(&self) -> u32 {
        let now = now_ms();
        let mut inner = self.inner.lock();
        let before = inner.map.len();
        inner.map.retain(|_, e| !is_expired(e, now));
        (before - inner.map.len()) as u32
    }

    // ── List ──────────────────────────────────────────────────────────────

    fn push(&self, key: &str, value: Vec<u8>, front: bool) -> Result<u32, ListError> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        if inner
            .map
            .get(key)
            .map(|e| is_expired(e, now))
            .unwrap_or(false)
        {
            inner.map.remove(key);
        }
        let rec = aof::rec_push(
            if front {
                aof::OP_PUSH_FRONT
            } else {
                aof::OP_PUSH_BACK
            },
            key,
            &value,
        );
        let len = apply_push(&mut inner.map, key, value, front)?;
        Self::journal(&mut inner, rec)?;
        Ok(len)
    }

    /// Prepend to a list (creating it if missing). Returns the new length.
    pub fn push_front(&self, key: &str, value: Vec<u8>) -> Result<u32, ListError> {
        self.push(key, value, true)
    }

    /// Append to a list (creating it if missing). Returns the new length.
    pub fn push_back(&self, key: &str, value: Vec<u8>) -> Result<u32, ListError> {
        self.push(key, value, false)
    }

    fn pop(&self, key: &str, front: bool) -> Result<Option<Vec<u8>>, ListError> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        if inner
            .map
            .get(key)
            .map(|e| is_expired(e, now))
            .unwrap_or(false)
        {
            inner.map.remove(key);
            return Ok(None);
        }
        let out = apply_pop(&mut inner.map, key, front)?;
        if out.is_some() {
            let op = if front {
                aof::OP_POP_FRONT
            } else {
                aof::OP_POP_BACK
            };
            Self::journal(&mut inner, aof::rec_key_only(op, key))?;
        }
        Ok(out)
    }

    /// Remove and return the first element, or `None` if empty/missing.
    pub fn pop_front(&self, key: &str) -> Result<Option<Vec<u8>>, ListError> {
        self.pop(key, true)
    }

    /// Remove and return the last element, or `None` if empty/missing.
    pub fn pop_back(&self, key: &str) -> Result<Option<Vec<u8>>, ListError> {
        self.pop(key, false)
    }

    /// List length (0 if missing). `WrongType` if the key holds bytes.
    pub fn llen(&self, key: &str) -> Result<u32, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        if inner
            .map
            .get(key)
            .map(|e| is_expired(e, now))
            .unwrap_or(false)
        {
            inner.map.remove(key);
            return Ok(0);
        }
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::List(l) => Ok(l.len() as u32),
                _ => Err(WrongType),
            },
            None => Ok(0),
        }
    }

    /// Elements in the inclusive range `[start, stop]`, Redis-style (negative
    /// indices count from the end; out-of-range is clamped).
    pub fn lrange(&self, key: &str, start: i64, stop: i64) -> Result<Vec<Vec<u8>>, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        if inner
            .map
            .get(key)
            .map(|e| is_expired(e, now))
            .unwrap_or(false)
        {
            inner.map.remove(key);
            return Ok(Vec::new());
        }
        match inner.map.get(key) {
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
                _ => Err(WrongType),
            },
            None => Ok(Vec::new()),
        }
    }

    // ── Hash ──────────────────────────────────────────────────────────────

    fn hash_expired(&self, inner: &mut Inner, key: &str, now: u64) {
        if inner
            .map
            .get(key)
            .map(|e| is_expired(e, now))
            .unwrap_or(false)
        {
            inner.map.remove(key);
        }
    }

    /// Set `field` in the hash at `key` (creating it). Returns true if the field
    /// was new. `WrongType` if the key holds a non-hash.
    pub fn hset(&self, key: &str, field: String, value: Vec<u8>) -> Result<bool, ListError> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        self.hash_expired(&mut inner, key, now);
        let rec = aof::rec_hset(key, &field, &value);
        let is_new = apply_hset(&mut inner.map, key, field, value)?;
        Self::journal(&mut inner, rec)?;
        Ok(is_new)
    }

    /// Get `field` from the hash at `key`. `None` if the key or field is missing.
    pub fn hget(&self, key: &str, field: &str) -> Result<Option<Vec<u8>>, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        self.hash_expired(&mut inner, key, now);
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::Hash(h) => Ok(h.get(field).cloned()),
                _ => Err(WrongType),
            },
            None => Ok(None),
        }
    }

    /// Delete `field`. Returns true if it existed. An emptied hash key is removed.
    pub fn hdel(&self, key: &str, field: &str) -> Result<bool, ListError> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        self.hash_expired(&mut inner, key, now);
        let existed = apply_hdel(&mut inner.map, key, field)?;
        if existed {
            Self::journal(&mut inner, aof::rec_hdel(key, field))?;
        }
        Ok(existed)
    }

    /// Whether `field` exists in the hash at `key`.
    pub fn hexists(&self, key: &str, field: &str) -> Result<bool, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        self.hash_expired(&mut inner, key, now);
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::Hash(h) => Ok(h.contains_key(field)),
                _ => Err(WrongType),
            },
            None => Ok(false),
        }
    }

    /// All field names (0 if missing). `WrongType` if the key holds a non-hash.
    pub fn hkeys(&self, key: &str) -> Result<Vec<String>, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        self.hash_expired(&mut inner, key, now);
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::Hash(h) => Ok(h.keys().cloned().collect()),
                _ => Err(WrongType),
            },
            None => Ok(Vec::new()),
        }
    }

    /// Number of fields (0 if missing). `WrongType` if the key holds a non-hash.
    pub fn hlen(&self, key: &str) -> Result<u32, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        self.hash_expired(&mut inner, key, now);
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::Hash(h) => Ok(h.len() as u32),
                _ => Err(WrongType),
            },
            None => Ok(0),
        }
    }

    /// All (field, value) pairs (empty if missing). `WrongType` on a non-hash.
    pub fn hgetall(&self, key: &str) -> Result<Vec<(String, Vec<u8>)>, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        self.hash_expired(&mut inner, key, now);
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::Hash(h) => Ok(h.iter().map(|(f, v)| (f.clone(), v.clone())).collect()),
                _ => Err(WrongType),
            },
            None => Ok(Vec::new()),
        }
    }

    // ── AOF maintenance ───────────────────────────────────────────────────

    /// Whether this store journals to a log.
    pub fn has_aof(&self) -> bool {
        self.inner.lock().aof.is_some()
    }

    /// Current log size in bytes (0 without a log).
    pub fn aof_size(&self) -> u64 {
        self.inner
            .lock()
            .aof
            .as_ref()
            .map(|a| a.size())
            .unwrap_or(0)
    }

    /// Rewrite the log as the shortest sequence that reproduces current state.
    ///
    /// A long-running queue appends forever (`push`, `pop`, `push`…) even when
    /// it holds two items; compaction collapses that history into the state
    /// itself. Written to a temp file and renamed, so a crash mid-compaction
    /// leaves the previous log intact.
    pub fn compact(&self) -> std::io::Result<u64> {
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        if inner.aof.is_none() {
            return Ok(0);
        }
        let now = now_ms();
        let mut records: Vec<Vec<u8>> = Vec::new();
        for (k, e) in inner.map.iter() {
            if is_expired(e, now) {
                continue; // don't journal what's already dead
            }
            match &e.value {
                Value::Bytes(b) => records.push(aof::rec_set(k, b, e.expire_at)),
                Value::List(l) => {
                    for elem in l {
                        records.push(aof::rec_push(aof::OP_PUSH_BACK, k, elem));
                    }
                    // push doesn't carry a TTL, so restore it explicitly.
                    if let Some(exp) = e.expire_at {
                        records.push(aof::rec_expire(k, exp));
                    }
                }
                Value::Hash(h) => {
                    for (field, val) in h {
                        records.push(aof::rec_hset(k, field, val));
                    }
                    // hset doesn't carry a TTL, so restore it explicitly.
                    if let Some(exp) = e.expire_at {
                        records.push(aof::rec_expire(k, exp));
                    }
                }
            }
        }
        let aof = inner.aof.as_mut().unwrap();
        aof.rewrite(&records)?;
        Ok(aof.size())
    }

    // ── Snapshot ──────────────────────────────────────────────────────────

    /// Serialize the whole store to a self-describing binary snapshot.
    ///
    /// Layout (little-endian):
    ///   "STRN" | version:u8 | flags:u8 | count:u32 | entries | crc32:u32
    ///   per entry: key_len:u32 | key | expire_at:u64 (0 = none) | type:u8 | body
    ///     type 0 (bytes): val_len:u32 | val
    ///     type 1 (list):  elem_count:u32 | (elem_len:u32 | elem)*
    /// The trailing CRC covers everything before it (v3+).
    pub fn dump_bytes(&self) -> Vec<u8> {
        let inner = self.inner.lock();
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.push(0u8); // flags (reserved)
        buf.extend_from_slice(&(inner.map.len() as u32).to_le_bytes());
        for (k, e) in inner.map.iter() {
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
                Value::Hash(h) => {
                    buf.push(TYPE_HASH);
                    buf.extend_from_slice(&(h.len() as u32).to_le_bytes());
                    for (field, val) in h {
                        let fb = field.as_bytes();
                        buf.extend_from_slice(&(fb.len() as u32).to_le_bytes());
                        buf.extend_from_slice(fb);
                        buf.extend_from_slice(&(val.len() as u32).to_le_bytes());
                        buf.extend_from_slice(val);
                    }
                }
            }
        }
        let checksum = crc32(&buf);
        buf.extend_from_slice(&checksum.to_le_bytes());
        buf
    }

    /// Load a snapshot, replacing all current state. Accepts versions 1–4.
    ///
    /// For v3 the trailing CRC is verified first, so a corrupted file is
    /// rejected instead of silently loading garbage.
    pub fn load_bytes(&self, data: &[u8]) -> Result<(), SnapshotError> {
        let bad = |m: &str| SnapshotError(m.to_string());
        if data.len() < 10 {
            return Err(bad("snapshot too small"));
        }
        if &data[0..4] != MAGIC {
            return Err(bad("bad magic: not a Strenor snapshot"));
        }
        let version = data[4];
        if version == 0 || version > VERSION {
            return Err(bad("unsupported snapshot version"));
        }

        // v3+ carries a trailing CRC over the preceding bytes.
        let body = if version >= 3 {
            if data.len() < 14 {
                return Err(bad("snapshot too small"));
            }
            let split = data.len() - 4;
            let want = u32::from_le_bytes(data[split..].try_into().unwrap());
            if crc32(&data[..split]) != want {
                return Err(bad("checksum mismatch: snapshot is corrupted"));
            }
            &data[..split]
        } else {
            data
        };

        let mut p = 6usize; // magic(4) + version(1) + flags(1)
        let read = |p: &mut usize, n: usize| -> Result<usize, SnapshotError> {
            if *p + n > body.len() {
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
        let count = u32_at(body, at) as usize;

        let mut next = HashMap::with_capacity(count);
        for _ in 0..count {
            let at = read(&mut p, 4)?;
            let kl = u32_at(body, at) as usize;
            let at = read(&mut p, kl)?;
            let key = String::from_utf8(body[at..at + kl].to_vec())
                .map_err(|_| bad("invalid utf8 key"))?;

            let at = read(&mut p, 8)?;
            let exp = u64_at(body, at);
            let expire_at = if exp == 0 { None } else { Some(exp) };

            // Version 1: bytes only, no type byte. Version 2+: type byte first.
            let kind = if version == 1 {
                TYPE_BYTES
            } else {
                let at = read(&mut p, 1)?;
                body[at]
            };

            let value = match kind {
                TYPE_BYTES => {
                    let at = read(&mut p, 4)?;
                    let vl = u32_at(body, at) as usize;
                    let at = read(&mut p, vl)?;
                    Value::Bytes(body[at..at + vl].to_vec())
                }
                TYPE_LIST => {
                    let at = read(&mut p, 4)?;
                    let n = u32_at(body, at) as usize;
                    let mut l = VecDeque::with_capacity(n);
                    for _ in 0..n {
                        let at = read(&mut p, 4)?;
                        let el = u32_at(body, at) as usize;
                        let at = read(&mut p, el)?;
                        l.push_back(body[at..at + el].to_vec());
                    }
                    Value::List(l)
                }
                TYPE_HASH => {
                    let at = read(&mut p, 4)?;
                    let n = u32_at(body, at) as usize;
                    let mut h = HashMap::with_capacity(n);
                    for _ in 0..n {
                        let at = read(&mut p, 4)?;
                        let fl = u32_at(body, at) as usize;
                        let at = read(&mut p, fl)?;
                        let field = String::from_utf8(body[at..at + fl].to_vec())
                            .map_err(|_| bad("invalid utf8 hash field"))?;
                        let at = read(&mut p, 4)?;
                        let vl = u32_at(body, at) as usize;
                        let at = read(&mut p, vl)?;
                        h.insert(field, body[at..at + vl].to_vec());
                    }
                    Value::Hash(h)
                }
                _ => return Err(bad("unknown entry type in snapshot")),
            };

            next.insert(key, Entry { value, expire_at });
        }

        self.inner.lock().map = next;
        Ok(())
    }
}

/// A list operation can fail on the structure type *or* on journalling.
#[derive(Debug)]
pub enum ListError {
    WrongType,
    Io(std::io::Error),
}

impl From<WrongType> for ListError {
    fn from(_: WrongType) -> Self {
        ListError::WrongType
    }
}

impl From<std::io::Error> for ListError {
    fn from(e: std::io::Error) -> Self {
        ListError::Io(e)
    }
}

impl std::fmt::Display for ListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ListError::WrongType => write!(f, "WRONGTYPE"),
            ListError::Io(e) => write!(f, "{e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::path::PathBuf;

    fn b(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    /// Unique temp path per test, cleaned on drop.
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Self {
            let mut p = std::env::temp_dir();
            let uniq = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            p.push(format!("strenor-{name}-{uniq}.aof"));
            Tmp(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn set_get_del() {
        let s = Store::new();
        s.set("a".into(), vec![1, 2, 3], None).unwrap();
        assert_eq!(s.get("a"), Ok(Some(vec![1, 2, 3])));
        assert!(s.exists("a"));
        assert!(s.del("a").unwrap());
        assert!(!s.del("a").unwrap());
        assert_eq!(s.get("a"), Ok(None));
    }

    #[test]
    fn ttl_semantics() {
        let s = Store::new();
        s.set("k".into(), vec![0], None).unwrap();
        assert_eq!(s.ttl("k"), -1);
        assert_eq!(s.ttl("ghost"), -2);
        assert!(s.expire("k", 10_000).unwrap());
        assert!(s.ttl("k") > 0);
        assert!(s.persist("k").unwrap());
        assert_eq!(s.ttl("k"), -1);
        assert!(!s.expire("missing", 1000).unwrap());
        assert!(!s.persist("missing").unwrap());
    }

    #[test]
    fn expiration_and_sweep() {
        let s = Store::new();
        s.set("x".into(), vec![0], Some(1)).unwrap();
        s.set("y".into(), vec![0], None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(s.get("x"), Ok(None));
        s.set("z".into(), vec![0], Some(1)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(s.sweep(), 1);
        assert!(s.exists("y"));
        assert_eq!(s.size(), 1);
        s.clear().unwrap();
        assert_eq!(s.size(), 0);
        assert!(s.keys().is_empty());
    }

    #[test]
    fn list_fifo_and_lifo() {
        let s = Store::new();
        assert_eq!(s.push_back("q", b("a")).unwrap(), 1);
        assert_eq!(s.push_back("q", b("b")).unwrap(), 2);
        assert_eq!(s.pop_front("q").unwrap(), Some(b("a")));
        assert_eq!(s.pop_front("q").unwrap(), Some(b("b")));
        assert_eq!(s.pop_front("q").unwrap(), None);
        assert!(!s.exists("q"));

        s.push_front("s", b("1")).unwrap();
        s.push_front("s", b("2")).unwrap();
        assert_eq!(s.pop_back("s").unwrap(), Some(b("1")));
        assert_eq!(s.pop_back("s").unwrap(), Some(b("2")));
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
        s.set("a".into(), b("hola"), None).unwrap();
        assert!(matches!(
            s.push_back("a", b("x")),
            Err(ListError::WrongType)
        ));
        assert!(matches!(s.pop_front("a"), Err(ListError::WrongType)));
        assert_eq!(s.llen("a"), Err(WrongType));
        assert_eq!(s.lrange("a", 0, -1), Err(WrongType));

        s.push_back("l", b("x")).unwrap();
        assert_eq!(s.get("l"), Err(WrongType));
        s.set("l".into(), b("now bytes"), None).unwrap();
        assert_eq!(s.get("l"), Ok(Some(b("now bytes"))));
    }

    #[test]
    fn snapshot_roundtrip_bytes_and_lists() {
        let s = Store::new();
        s.set("name".into(), b("strenor"), None).unwrap();
        s.set("ttl".into(), vec![1], Some(60_000)).unwrap();
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
    fn snapshot_rejects_garbage_and_corruption() {
        let s = Store::new();
        assert!(s.load_bytes(b"not a snapshot").is_err());
        assert!(s.load_bytes(b"STRN\x09\x00\x00\x00\x00\x00").is_err()); // bad version

        // Flip a byte in the middle: the trailing CRC must catch it.
        s.set("k".into(), b("value"), None).unwrap();
        let mut bytes = s.dump_bytes();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        let fresh = Store::new();
        let err = fresh.load_bytes(&bytes).unwrap_err();
        assert!(err.0.contains("corrupted"), "got: {}", err.0);
    }

    #[test]
    fn aof_persists_and_replays() {
        let tmp = Tmp::new("replay");
        {
            let (s, rec) = Store::with_aof(tmp.path(), false).unwrap();
            assert_eq!(rec, Recovery::default()); // fresh file: nothing to apply
            s.set("user".into(), b("brashkie"), None).unwrap();
            s.set("gone".into(), b("x"), None).unwrap();
            s.del("gone").unwrap();
            s.push_back("jobs", b("j1")).unwrap();
            s.push_back("jobs", b("j2")).unwrap();
            s.pop_front("jobs").unwrap();
            s.expire("user", 60_000).unwrap();
        } // dropped: simulates the process going away

        let (s2, rec) = Store::with_aof(tmp.path(), false).unwrap();
        assert!(!rec.truncated);
        assert_eq!(rec.applied, 7);
        assert_eq!(s2.get("user"), Ok(Some(b("brashkie"))));
        assert_eq!(s2.get("gone"), Ok(None));
        assert_eq!(s2.lrange("jobs", 0, -1), Ok(vec![b("j2")]));
        assert!(s2.ttl("user") > 0);
    }

    #[test]
    fn aof_recovers_from_a_torn_tail() {
        let tmp = Tmp::new("torn");
        {
            let (s, _) = Store::with_aof(tmp.path(), false).unwrap();
            s.set("good".into(), b("1"), None).unwrap();
            s.set("also-good".into(), b("2"), None).unwrap();
        }
        // Simulate a crash mid-append: garbage bytes at the end.
        {
            use std::io::Write;
            let mut f = OpenOptions::new().append(true).open(tmp.path()).unwrap();
            f.write_all(&[0xff, 0x00, 0x00, 0x00, 0xde, 0xad]).unwrap();
        }

        let (s2, rec) = Store::with_aof(tmp.path(), false).unwrap();
        assert!(rec.truncated, "a torn tail must be reported");
        assert_eq!(rec.applied, 2); // both intact records survived
        assert_eq!(s2.get("good"), Ok(Some(b("1"))));
        assert_eq!(s2.get("also-good"), Ok(Some(b("2"))));

        // The file was truncated, so reopening is now clean.
        drop(s2);
        let (_, rec2) = Store::with_aof(tmp.path(), false).unwrap();
        assert!(!rec2.truncated);
        assert_eq!(rec2.applied, 2);
    }

    #[test]
    fn aof_compaction_shrinks_and_preserves_state() {
        let tmp = Tmp::new("compact");
        let (s, _) = Store::with_aof(tmp.path(), false).unwrap();
        // Churn: a queue that pushes and pops forever grows the log unboundedly.
        for i in 0..200 {
            s.push_back("q", b(&format!("job-{i}"))).unwrap();
            s.pop_front("q").unwrap();
        }
        s.set("keep".into(), b("value"), None).unwrap();
        s.push_back("q", b("last")).unwrap();
        s.push_back("ttl-list", b("e")).unwrap();
        s.expire("ttl-list", 60_000).unwrap();

        let before = s.aof_size();
        let after = s.compact().unwrap();
        assert!(
            after < before,
            "compaction must shrink: {after} !< {before}"
        );

        // State survives a reopen from the compacted log.
        drop(s);
        let (s2, rec) = Store::with_aof(tmp.path(), false).unwrap();
        assert!(!rec.truncated);
        assert_eq!(s2.get("keep"), Ok(Some(b("value"))));
        assert_eq!(s2.lrange("q", 0, -1), Ok(vec![b("last")]));
        assert!(s2.ttl("ttl-list") > 0); // list TTL restored
    }

    #[test]
    fn aof_skips_expired_entries_on_compaction() {
        let tmp = Tmp::new("expired");
        let (s, _) = Store::with_aof(tmp.path(), false).unwrap();
        s.set("dead".into(), b("x"), Some(1)).unwrap();
        s.set("alive".into(), b("y"), None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        s.compact().unwrap();
        drop(s);

        let (s2, _) = Store::with_aof(tmp.path(), false).unwrap();
        assert_eq!(s2.get("alive"), Ok(Some(b("y"))));
        assert_eq!(s2.get("dead"), Ok(None));
        assert_eq!(s2.size(), 1); // the dead key was never journalled
    }

    #[test]
    fn store_without_aof_reports_no_log() {
        let s = Store::new();
        assert!(!s.has_aof());
        assert_eq!(s.aof_size(), 0);
        assert_eq!(s.compact().unwrap(), 0); // no-op, not an error
    }

    #[test]
    fn close_releases_the_log_and_is_idempotent() {
        let tmp = Tmp::new("close");
        let (s, _) = Store::with_aof(tmp.path(), false).unwrap();
        s.set("k".into(), b("v"), None).unwrap();
        assert!(s.has_aof());

        s.close().unwrap();
        assert!(s.is_closed());
        assert!(!s.has_aof()); // handle released
        s.close().unwrap(); // idempotent

        // Reads still work from memory...
        assert_eq!(s.get("k"), Ok(Some(b("v"))));
        // ...but writes must fail loudly instead of skipping the journal.
        assert!(s.set("k2".into(), b("v"), None).is_err());
        assert!(s.del("k").is_err());
        assert!(s.clear().is_err());
        assert!(s.expire("k", 100).is_err());
        assert!(s.persist("k").is_err());
        assert!(matches!(s.push_back("l", b("x")), Err(ListError::Io(_))));
        assert!(matches!(s.pop_front("l"), Err(ListError::Io(_))));
        assert!(s.compact().is_err());
    }

    #[test]
    fn closed_log_can_be_deleted_and_reopened() {
        // The exact Windows failure: a still-open handle leaves the file in a
        // "delete pending" state, and the next open fails with Access Denied.
        let tmp = Tmp::new("reopen");
        {
            let (s, _) = Store::with_aof(tmp.path(), false).unwrap();
            s.set("a".into(), b("1"), None).unwrap();
            s.close().unwrap();
        }
        std::fs::remove_file(tmp.path()).unwrap(); // must succeed: no open handle

        let (fresh, rec) = Store::with_aof(tmp.path(), false).unwrap();
        assert_eq!(rec.applied, 0); // brand-new log
        fresh.set("b".into(), b("2"), None).unwrap();
        assert_eq!(fresh.get("b"), Ok(Some(b("2"))));
        fresh.close().unwrap();
    }

    #[test]
    fn memory_only_store_closes_cleanly() {
        let s = Store::new();
        s.set("k".into(), b("v"), None).unwrap();
        s.close().unwrap();
        assert!(s.is_closed());
        assert_eq!(s.get("k"), Ok(Some(b("v"))));
        assert!(s.set("x".into(), b("y"), None).is_err());
    }

    #[test]
    fn hash_basic_ops() {
        let s = Store::new();
        assert!(s.hset("u:1", "name".into(), b("alice")).unwrap()); // new field
        assert!(!s.hset("u:1", "name".into(), b("bob")).unwrap()); // overwrite
        assert_eq!(s.hget("u:1", "name").unwrap(), Some(b("bob")));
        assert_eq!(s.hget("u:1", "missing").unwrap(), None);
        assert_eq!(s.hget("missing", "x").unwrap(), None);

        s.hset("u:1", "age".into(), b("20")).unwrap();
        assert_eq!(s.hlen("u:1"), Ok(2));
        assert!(s.hexists("u:1", "age").unwrap());
        assert!(!s.hexists("u:1", "nope").unwrap());
        let mut ks = s.hkeys("u:1").unwrap();
        ks.sort();
        assert_eq!(ks, vec!["age".to_string(), "name".to_string()]);

        assert!(s.hdel("u:1", "age").unwrap());
        assert!(!s.hdel("u:1", "age").unwrap());
        assert_eq!(s.hlen("u:1"), Ok(1));
    }

    #[test]
    fn hash_empty_key_is_removed() {
        let s = Store::new();
        s.hset("h", "only".into(), b("v")).unwrap();
        assert!(s.exists("h"));
        s.hdel("h", "only").unwrap();
        assert!(!s.exists("h")); // last field gone -> key deleted
        assert_eq!(s.hlen("h"), Ok(0));
        assert_eq!(s.hgetall("h").unwrap(), Vec::new());
        assert!(s.hkeys("h").unwrap().is_empty());
    }

    #[test]
    fn hash_hgetall() {
        let s = Store::new();
        s.hset("cfg", "a".into(), b("1")).unwrap();
        s.hset("cfg", "b".into(), b("2")).unwrap();
        let mut all = s.hgetall("cfg").unwrap();
        all.sort();
        assert_eq!(all, vec![("a".into(), b("1")), ("b".into(), b("2"))]);
    }

    #[test]
    fn hash_wrong_type() {
        let s = Store::new();
        s.set("str".into(), b("hola"), None).unwrap();
        assert!(matches!(
            s.hset("str", "f".into(), b("x")),
            Err(ListError::WrongType)
        ));
        assert!(matches!(s.hdel("str", "f"), Err(ListError::WrongType)));
        assert_eq!(s.hget("str", "f"), Err(WrongType));
        assert_eq!(s.hexists("str", "f"), Err(WrongType));
        assert_eq!(s.hkeys("str"), Err(WrongType));
        assert_eq!(s.hlen("str"), Err(WrongType));
        assert_eq!(s.hgetall("str"), Err(WrongType));

        // And a hash is not a string/list.
        s.hset("h", "f".into(), b("v")).unwrap();
        assert_eq!(s.get("h"), Err(WrongType));
        assert!(matches!(
            s.push_back("h", b("x")),
            Err(ListError::WrongType)
        ));
    }

    #[test]
    fn hash_honors_ttl() {
        let s = Store::new();
        s.hset("sess", "step".into(), b("menu")).unwrap();
        assert!(s.expire("sess", 60_000).unwrap());
        assert!(s.ttl("sess") > 0);
        s.set("k".into(), b("v"), Some(1)).unwrap();

        s.hset("dead", "f".into(), b("v")).unwrap();
        s.expire("dead", 1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(s.hget("dead", "f").unwrap(), None); // lazily expired
        assert_eq!(s.hlen("dead"), Ok(0));
    }

    #[test]
    fn hash_snapshot_roundtrip() {
        let s = Store::new();
        s.hset("u:1", "name".into(), b("alice")).unwrap();
        s.hset("u:1", "age".into(), b("20")).unwrap();
        s.set("plain".into(), b("x"), None).unwrap();
        let bytes = s.dump_bytes();

        let fresh = Store::new();
        fresh.load_bytes(&bytes).unwrap();
        assert_eq!(fresh.hget("u:1", "name").unwrap(), Some(b("alice")));
        assert_eq!(fresh.hget("u:1", "age").unwrap(), Some(b("20")));
        assert_eq!(fresh.hlen("u:1"), Ok(2));
        assert_eq!(fresh.get("plain"), Ok(Some(b("x"))));
    }

    #[test]
    fn hash_aof_replay_and_compaction() {
        let tmp = Tmp::new("hash-aof");
        {
            let (s, _) = Store::with_aof(tmp.path(), false).unwrap();
            s.hset("u:1", "name".into(), b("alice")).unwrap();
            s.hset("u:1", "tmp".into(), b("x")).unwrap();
            s.hdel("u:1", "tmp").unwrap();
            // Churn to grow the log.
            for i in 0..100 {
                s.hset("u:1", "counter".into(), b(&i.to_string())).unwrap();
            }
            s.expire("u:1", 60_000).unwrap();
            s.close().unwrap();
        }

        let (s2, rec) = Store::with_aof(tmp.path(), false).unwrap();
        assert!(!rec.truncated);
        assert_eq!(s2.hget("u:1", "name").unwrap(), Some(b("alice")));
        assert_eq!(s2.hget("u:1", "tmp").unwrap(), None); // deleted field stays deleted
        assert_eq!(s2.hget("u:1", "counter").unwrap(), Some(b("99")));
        assert!(s2.ttl("u:1") > 0);

        let before = s2.aof_size();
        s2.compact().unwrap();
        let after = s2.aof_size();
        assert!(after < before);
        s2.close().unwrap();

        // State (and TTL) survive reopening from the compacted log.
        let (s3, _) = Store::with_aof(tmp.path(), false).unwrap();
        assert_eq!(s3.hget("u:1", "counter").unwrap(), Some(b("99")));
        assert_eq!(s3.hlen("u:1"), Ok(2)); // name + counter
        assert!(s3.ttl("u:1") > 0);
        s3.close().unwrap();
    }
}
