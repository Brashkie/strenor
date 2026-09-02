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
mod zset;

use zset::{Scored, ZSet};

use aof::Aof;
pub use aof::{crc32, write_atomic};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Snapshot magic and current format version.
/// - v1: bytes only.
/// - v2: adds a structure-type byte per entry (lists).
/// - v3: adds a trailing CRC-32 over the body (corruption detection).
/// - v4: adds the hash structure type.
/// - v5: adds the set structure type.
/// - v6: adds the sorted-set structure type.
///
/// Older versions still load, so existing snapshots keep working.
pub const MAGIC: &[u8; 4] = b"STRN";
pub const VERSION: u8 = 6;

const TYPE_BYTES: u8 = 0;
const TYPE_LIST: u8 = 1;
const TYPE_HASH: u8 = 2;
const TYPE_SET: u8 = 3;
const TYPE_ZSET: u8 = 4;

/// A structure-typed value. The engine distinguishes these; it never looks
/// inside a blob. New engine structures (Hash, Set, SortedSet) will be added
/// here in later versions.
#[derive(Clone)]
enum Value {
    Bytes(Vec<u8>),
    List(VecDeque<Vec<u8>>),
    Hash(HashMap<String, Vec<u8>>),
    Set(HashSet<Vec<u8>>),
    ZSet(ZSet),
}

/// A stored record: a typed value plus an optional expiration (TTL applies to
/// any structure type, exactly like Redis).
#[derive(Clone)]
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
    /// When a transaction is open, journalled records accumulate here instead of
    /// hitting the log, then flush as one atomic batch on commit (or are dropped
    /// on rollback). `None` means no transaction is in progress.
    tx_buffer: Option<Vec<Vec<u8>>>,
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

fn apply_sadd(
    map: &mut HashMap<String, Entry>,
    key: &str,
    member: Vec<u8>,
) -> Result<bool, WrongType> {
    match map.get_mut(key) {
        Some(e) => match &mut e.value {
            Value::Set(set) => Ok(set.insert(member)),
            _ => Err(WrongType),
        },
        None => {
            let mut set = HashSet::new();
            set.insert(member);
            map.insert(
                key.to_string(),
                Entry {
                    value: Value::Set(set),
                    expire_at: None,
                },
            );
            Ok(true)
        }
    }
}

fn apply_srem(
    map: &mut HashMap<String, Entry>,
    key: &str,
    member: &[u8],
) -> Result<bool, WrongType> {
    match map.get_mut(key) {
        Some(e) => match &mut e.value {
            Value::Set(set) => {
                let removed = set.remove(member);
                if set.is_empty() {
                    map.remove(key); // an empty set key is deleted (Redis-like)
                }
                Ok(removed)
            }
            _ => Err(WrongType),
        },
        None => Ok(false),
    }
}

/// Score rejected because it is NaN or the result is non-finite.
#[derive(Debug, PartialEq, Eq)]
pub struct BadScore;

fn apply_zadd(
    map: &mut HashMap<String, Entry>,
    key: &str,
    score: f64,
    member: Vec<u8>,
) -> Result<bool, ZAddError> {
    match map.get_mut(key) {
        Some(e) => match &mut e.value {
            Value::ZSet(z) => z.add(member, score).map_err(|_| ZAddError::BadScore),
            _ => Err(ZAddError::WrongType),
        },
        None => {
            let mut z = ZSet::new();
            let is_new = z.add(member, score).map_err(|_| ZAddError::BadScore)?;
            map.insert(
                key.to_string(),
                Entry {
                    value: Value::ZSet(z),
                    expire_at: None,
                },
            );
            Ok(is_new)
        }
    }
}

fn apply_zrem(
    map: &mut HashMap<String, Entry>,
    key: &str,
    member: &[u8],
) -> Result<bool, WrongType> {
    match map.get_mut(key) {
        Some(e) => match &mut e.value {
            Value::ZSet(z) => {
                let removed = z.remove(member);
                if z.is_empty() {
                    map.remove(key); // an empty zset key is deleted (Redis-like)
                }
                Ok(removed)
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
    /// Undo-log for the open transaction, if any. Maps each key touched during
    /// the transaction to its state *before* the transaction began: `Some(entry)`
    /// if it existed, `None` if it didn't. Only the first change to a key is
    /// recorded, so rollback restores the pre-transaction value. This is
    /// O(keys changed), not O(state size) — the whole point of the redesign.
    tx_undo: Mutex<Option<HashMap<String, Option<Entry>>>>,
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
                tx_buffer: None,
            }),
            tx_undo: Mutex::new(None),
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
                tx_buffer: None,
            }),
            tx_undo: Mutex::new(None),
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
            aof::OP_SADD => match (c.string(), c.bytes()) {
                (Some(k), Some(m)) => {
                    let _ = apply_sadd(map, &k, m.to_vec());
                    true
                }
                _ => false,
            },
            aof::OP_SREM => match (c.string(), c.bytes()) {
                (Some(k), Some(m)) => {
                    let _ = apply_srem(map, &k, m);
                    true
                }
                _ => false,
            },
            aof::OP_ZADD => match (c.string(), c.f64(), c.bytes()) {
                (Some(k), Some(score), Some(m)) => {
                    let _ = apply_zadd(map, &k, score, m.to_vec());
                    true
                }
                _ => false,
            },
            aof::OP_ZREM => match (c.string(), c.bytes()) {
                (Some(k), Some(m)) => {
                    let _ = apply_zrem(map, &k, m);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Journal a record if a log is attached. Errors are surfaced: a write that
    /// isn't durable must not be reported as successful.
    ///
    /// Inside a transaction, records are staged in `tx_buffer` and written as one
    /// atomic batch on commit — never partially — so a rollback leaves the log
    /// exactly as it was.
    fn journal(inner: &mut Inner, payload: Vec<u8>) -> std::io::Result<()> {
        if let Some(buf) = inner.tx_buffer.as_mut() {
            buf.push(payload);
            return Ok(());
        }
        match inner.aof.as_mut() {
            Some(a) => a.append(&payload),
            None => Ok(()),
        }
    }

    /// Before a mutation touches `key` inside a transaction, record its
    /// pre-transaction state in the undo-log — but only the first time the key is
    /// touched, so rollback restores the value as it was before `tx_begin`.
    ///
    /// No transaction open → does nothing. This is what makes rollback
    /// O(keys changed) instead of O(state size): we clone one entry per changed
    /// key, lazily, rather than the whole map up front.
    fn record_undo(&self, inner: &Inner, key: &str) {
        let mut undo_guard = self.tx_undo.lock();
        if let Some(undo) = undo_guard.as_mut() {
            if !undo.contains_key(key) {
                undo.insert(key.to_string(), inner.map.get(key).cloned());
            }
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
        self.record_undo(&inner, &key);
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
        self.record_undo(&inner, key);
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
        self.record_undo(&inner, key);
        let ok = apply_expire(&mut inner.map, key, expire_at);
        if ok {
            Self::journal(&mut inner, aof::rec_expire(key, expire_at))?;
        }
        Ok(ok)
    }

    pub fn persist(&self, key: &str) -> std::io::Result<bool> {
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        self.record_undo(&inner, key);
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
        // Inside a transaction, `clear` touches every key, so the undo-log must
        // capture them all to be able to roll back. This one operation is
        // unavoidably O(state) — but only clear, and only within a transaction.
        {
            let mut undo_guard = self.tx_undo.lock();
            if let Some(undo) = undo_guard.as_mut() {
                for (k, e) in inner.map.iter() {
                    undo.entry(k.clone()).or_insert_with(|| Some(e.clone()));
                }
            }
        }
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
        self.record_undo(&inner, key);
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
        self.record_undo(&inner, key);
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
        self.record_undo(&inner, key);
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
        self.record_undo(&inner, key);
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

    // ── Set ───────────────────────────────────────────────────────────────

    /// Add `member` to the set at `key` (creating it). Returns true if it was
    /// new. `WrongType` if the key holds a non-set.
    pub fn sadd(&self, key: &str, member: Vec<u8>) -> Result<bool, ListError> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        self.hash_expired(&mut inner, key, now);
        self.record_undo(&inner, key);
        let rec = aof::rec_smember(aof::OP_SADD, key, &member);
        let added = apply_sadd(&mut inner.map, key, member)?;
        Self::journal(&mut inner, rec)?;
        Ok(added)
    }

    /// Remove `member`. Returns true if it was present. An emptied set is removed.
    pub fn srem(&self, key: &str, member: &[u8]) -> Result<bool, ListError> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        self.hash_expired(&mut inner, key, now);
        self.record_undo(&inner, key);
        let removed = apply_srem(&mut inner.map, key, member)?;
        if removed {
            Self::journal(&mut inner, aof::rec_smember(aof::OP_SREM, key, member))?;
        }
        Ok(removed)
    }

    /// Whether `member` is in the set at `key`.
    pub fn sismember(&self, key: &str, member: &[u8]) -> Result<bool, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        self.hash_expired(&mut inner, key, now);
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::Set(set) => Ok(set.contains(member)),
                _ => Err(WrongType),
            },
            None => Ok(false),
        }
    }

    /// All members (empty if missing). Order is unspecified. `WrongType` on a
    /// non-set.
    pub fn smembers(&self, key: &str) -> Result<Vec<Vec<u8>>, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        self.hash_expired(&mut inner, key, now);
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::Set(set) => Ok(set.iter().cloned().collect()),
                _ => Err(WrongType),
            },
            None => Ok(Vec::new()),
        }
    }

    /// Cardinality — number of members (0 if missing). `WrongType` on a non-set.
    pub fn scard(&self, key: &str) -> Result<u32, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        self.hash_expired(&mut inner, key, now);
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::Set(set) => Ok(set.len() as u32),
                _ => Err(WrongType),
            },
            None => Ok(0),
        }
    }

    // ── Sorted set ────────────────────────────────────────────────────────

    /// Add or update `member` with `score` (creating the zset). Returns true if
    /// the member is new. `WrongType` on a non-zset, `BadScore` if `score` is NaN.
    pub fn zadd(&self, key: &str, score: f64, member: Vec<u8>) -> Result<bool, ZAddError> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        self.hash_expired(&mut inner, key, now);
        self.record_undo(&inner, key);
        let rec = aof::rec_zadd(key, score, &member);
        let is_new = apply_zadd(&mut inner.map, key, score, member)?;
        Self::journal(&mut inner, rec)?;
        Ok(is_new)
    }

    /// Add `delta` to a member's score (creating it at `delta`). Returns the new
    /// score. `WrongType` on a non-zset, `BadScore` on a non-finite result.
    pub fn zincrby(&self, key: &str, delta: f64, member: Vec<u8>) -> Result<f64, ZAddError> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        self.hash_expired(&mut inner, key, now);
        self.record_undo(&inner, key);
        let new_score = match inner.map.get_mut(key) {
            Some(e) => match &mut e.value {
                Value::ZSet(z) => z
                    .incr_by(member.clone(), delta)
                    .map_err(|_| ZAddError::BadScore)?,
                _ => return Err(ZAddError::WrongType),
            },
            None => {
                if delta.is_nan() {
                    return Err(ZAddError::BadScore);
                }
                let mut z = ZSet::new();
                z.add(member.clone(), delta)
                    .map_err(|_| ZAddError::BadScore)?;
                inner.map.insert(
                    key.to_string(),
                    Entry {
                        value: Value::ZSet(z),
                        expire_at: None,
                    },
                );
                delta
            }
        };
        // Journal the resulting absolute score so replay is deterministic.
        Self::journal(&mut inner, aof::rec_zadd(key, new_score, &member))?;
        Ok(new_score)
    }

    /// Remove `member`. Returns true if present. An emptied zset key is removed.
    pub fn zrem(&self, key: &str, member: &[u8]) -> Result<bool, ListError> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        self.hash_expired(&mut inner, key, now);
        self.record_undo(&inner, key);
        let removed = apply_zrem(&mut inner.map, key, member)?;
        if removed {
            Self::journal(&mut inner, aof::rec_zrem(key, member))?;
        }
        Ok(removed)
    }

    /// The score of `member`, or `None` if the member/key is missing.
    pub fn zscore(&self, key: &str, member: &[u8]) -> Result<Option<f64>, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        self.hash_expired(&mut inner, key, now);
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::ZSet(z) => Ok(z.score(member)),
                _ => Err(WrongType),
            },
            None => Ok(None),
        }
    }

    /// 0-based rank of `member` (low score first), or `None` if missing.
    pub fn zrank(&self, key: &str, member: &[u8]) -> Result<Option<u32>, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        self.hash_expired(&mut inner, key, now);
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::ZSet(z) => Ok(z.rank(member)),
                _ => Err(WrongType),
            },
            None => Ok(None),
        }
    }

    /// Cardinality — number of members (0 if missing). `WrongType` on a non-zset.
    pub fn zcard(&self, key: &str) -> Result<u32, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        self.hash_expired(&mut inner, key, now);
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::ZSet(z) => Ok(z.len() as u32),
                _ => Err(WrongType),
            },
            None => Ok(0),
        }
    }

    /// Members in the inclusive rank range `[start, stop]`, low score first.
    pub fn zrange(&self, key: &str, start: i64, stop: i64) -> Result<Vec<Vec<u8>>, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        self.hash_expired(&mut inner, key, now);
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::ZSet(z) => Ok(z.range(start, stop)),
                _ => Err(WrongType),
            },
            None => Ok(Vec::new()),
        }
    }

    /// Same as `zrange`, but each element carries its score.
    pub fn zrange_scored(
        &self,
        key: &str,
        start: i64,
        stop: i64,
    ) -> Result<Vec<Scored>, WrongType> {
        let now = now_ms();
        let mut inner = self.inner.lock();
        self.hash_expired(&mut inner, key, now);
        match inner.map.get(key) {
            Some(e) => match &e.value {
                Value::ZSet(z) => Ok(z.range_scored(start, stop)),
                _ => Err(WrongType),
            },
            None => Ok(Vec::new()),
        }
    }

    // ── Transactions & batches ────────────────────────────────────────────

    /// Run `f` as an all-or-nothing transaction.
    ///
    /// A snapshot of the state is taken up front; `f` runs against the live store
    /// with journalling staged in a buffer. If `f` returns `Ok`, the staged
    /// records are written to the log as one atomic batch and the changes stand.
    /// If `f` returns `Err` (or a WRONGTYPE/BadScore surfaces as one), the state
    /// is restored from the snapshot and nothing is written to the log.
    ///
    /// Nested transactions aren't supported — calling this while one is open
    /// returns an error. The rollback snapshot is O(state size); fine for
    /// thousands of keys, not intended for millions.
    ///
    /// Note: the closure takes `&Store`, so it calls the normal methods (`set`,
    /// `hset`, …). Because the whole thing runs under the store's lock, those
    /// calls must not deadlock — this is why the JS layer drives commit/rollback
    /// through the explicit `tx_begin`/`tx_commit`/`tx_rollback` methods instead.
    pub fn transact<F, T, E>(&self, f: F) -> Result<T, TxError<E>>
    where
        F: FnOnce(&Store) -> Result<T, E>,
    {
        self.tx_begin().map_err(TxError::Io)?;
        // A panic in `f` would leave a transaction open; guard against it by
        // catching unwind and rolling back before re-panicking.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match result {
            Ok(Ok(value)) => {
                self.tx_commit().map_err(TxError::Io)?;
                Ok(value)
            }
            Ok(Err(user_err)) => {
                self.tx_rollback().map_err(TxError::Io)?;
                Err(TxError::User(user_err))
            }
            Err(panic) => {
                let _ = self.tx_rollback();
                std::panic::resume_unwind(panic);
            }
        }
    }

    /// Begin a transaction: start an empty undo-log and stage journal records.
    /// Errors if the store is closed or a transaction is already open.
    ///
    /// Unlike the previous full-snapshot approach, begin is now O(1) — it just
    /// arms the undo-log. Per-key previous state is captured lazily, only for
    /// keys the transaction actually touches.
    pub fn tx_begin(&self) -> std::io::Result<()> {
        let mut inner = self.inner.lock();
        Self::ensure_open(&inner)?;
        if inner.tx_buffer.is_some() {
            return Err(std::io::Error::other("a transaction is already open"));
        }
        inner.tx_buffer = Some(Vec::new());
        *self.tx_undo.lock() = Some(HashMap::new());
        Ok(())
    }

    /// Commit the open transaction: flush staged records as one atomic batch and
    /// discard the undo-log. O(records written), independent of state size.
    pub fn tx_commit(&self) -> std::io::Result<()> {
        let mut inner = self.inner.lock();
        let staged = match inner.tx_buffer.take() {
            Some(s) => s,
            None => return Err(std::io::Error::other("no transaction is open")),
        };
        *self.tx_undo.lock() = None; // drop the undo-log: changes are permanent
        if let Some(a) = inner.aof.as_mut() {
            a.append_batch(&staged)?;
        }
        Ok(())
    }

    /// Roll back the open transaction: undo each recorded change and discard all
    /// staged records (nothing reaches the log). O(keys changed), not O(state).
    pub fn tx_rollback(&self) -> std::io::Result<()> {
        let mut inner = self.inner.lock();
        if inner.tx_buffer.take().is_none() {
            return Err(std::io::Error::other("no transaction is open"));
        }
        if let Some(undo) = self.tx_undo.lock().take() {
            // Restore each touched key to its pre-transaction state. `Some(entry)`
            // means it existed and is put back; `None` means it didn't exist and
            // is removed. Applied in any order — each key is independent.
            for (key, prev) in undo {
                match prev {
                    Some(entry) => {
                        inner.map.insert(key, entry);
                    }
                    None => {
                        inner.map.remove(&key);
                    }
                }
            }
        }
        Ok(())
    }

    /// Whether a transaction is currently open.
    pub fn in_transaction(&self) -> bool {
        self.inner.lock().tx_buffer.is_some()
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
                Value::Set(set) => {
                    for member in set {
                        records.push(aof::rec_smember(aof::OP_SADD, k, member));
                    }
                    // sadd doesn't carry a TTL, so restore it explicitly.
                    if let Some(exp) = e.expire_at {
                        records.push(aof::rec_expire(k, exp));
                    }
                }
                Value::ZSet(z) => {
                    for (member, score) in z.entries() {
                        records.push(aof::rec_zadd(k, score, member));
                    }
                    // zadd doesn't carry a TTL, so restore it explicitly.
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
        Self::dump_locked(&inner)
    }

    /// Serialize `inner`'s map. Shared by `dump_bytes` and the transaction
    /// rollback snapshot, so both use the exact same versioned format.
    fn dump_locked(inner: &Inner) -> Vec<u8> {
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
                Value::Set(set) => {
                    buf.push(TYPE_SET);
                    buf.extend_from_slice(&(set.len() as u32).to_le_bytes());
                    for member in set {
                        buf.extend_from_slice(&(member.len() as u32).to_le_bytes());
                        buf.extend_from_slice(member);
                    }
                }
                Value::ZSet(z) => {
                    buf.push(TYPE_ZSET);
                    buf.extend_from_slice(&(z.len() as u32).to_le_bytes());
                    for (member, score) in z.entries() {
                        buf.extend_from_slice(&score.to_le_bytes());
                        buf.extend_from_slice(&(member.len() as u32).to_le_bytes());
                        buf.extend_from_slice(member);
                    }
                }
            }
        }
        let checksum = crc32(&buf);
        buf.extend_from_slice(&checksum.to_le_bytes());
        buf
    }

    /// Load a snapshot, replacing all current state. Accepts versions 1–6.
    ///
    /// For v3 the trailing CRC is verified first, so a corrupted file is
    /// rejected instead of silently loading garbage.
    pub fn load_bytes(&self, data: &[u8]) -> Result<(), SnapshotError> {
        let map = Self::parse_snapshot(data)?;
        self.inner.lock().map = map;
        Ok(())
    }

    /// Parse a snapshot into a fresh map without touching any store state.
    fn parse_snapshot(data: &[u8]) -> Result<HashMap<String, Entry>, SnapshotError> {
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

        // Never trust a length field from a file: a corrupt `count` (e.g. 4
        // billion) would make `with_capacity` try to reserve gigabytes and abort
        // the process before any per-entry validation runs. Every entry needs at
        // least ~6 bytes on disk, so the real count can't exceed body/6. Reserve
        // the smaller of the two; if the stored count lied high, the loop still
        // fails cleanly in `read()` with "truncated".
        let cap = count.min(body.len() / 6 + 1);
        let mut next = HashMap::with_capacity(cap);
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
                    let mut l = VecDeque::with_capacity(n.min(body.len() / 4 + 1));
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
                    let mut h = HashMap::with_capacity(n.min(body.len() / 4 + 1));
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
                TYPE_SET => {
                    let at = read(&mut p, 4)?;
                    let n = u32_at(body, at) as usize;
                    let mut set = HashSet::with_capacity(n.min(body.len() / 4 + 1));
                    for _ in 0..n {
                        let at = read(&mut p, 4)?;
                        let ml = u32_at(body, at) as usize;
                        let at = read(&mut p, ml)?;
                        set.insert(body[at..at + ml].to_vec());
                    }
                    Value::Set(set)
                }
                TYPE_ZSET => {
                    let at = read(&mut p, 4)?;
                    let n = u32_at(body, at) as usize;
                    let mut z = ZSet::new();
                    for _ in 0..n {
                        let at = read(&mut p, 8)?;
                        let score = f64::from_le_bytes(body[at..at + 8].try_into().unwrap());
                        let at = read(&mut p, 4)?;
                        let ml = u32_at(body, at) as usize;
                        let at = read(&mut p, ml)?;
                        // Snapshots only ever contain finite scores, so this can't fail.
                        let _ = z.add(body[at..at + ml].to_vec(), score);
                    }
                    Value::ZSet(z)
                }
                _ => return Err(bad("unknown entry type in snapshot")),
            };

            next.insert(key, Entry { value, expire_at });
        }

        Ok(next)
    }
}

/// A transaction fails either because the user's closure returned an error
/// (`User`) or because journalling the commit/rollback hit IO (`Io`).
#[derive(Debug)]
pub enum TxError<E> {
    User(E),
    Io(std::io::Error),
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

/// `zadd`/`zincrby` can fail on the structure type, on a bad score (NaN or a
/// non-finite result), or on journalling.
#[derive(Debug)]
pub enum ZAddError {
    WrongType,
    BadScore,
    Io(std::io::Error),
}

impl From<std::io::Error> for ZAddError {
    fn from(e: std::io::Error) -> Self {
        ZAddError::Io(e)
    }
}

impl std::fmt::Display for ZAddError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ZAddError::WrongType => write!(f, "WRONGTYPE"),
            ZAddError::BadScore => write!(f, "score is not a valid number"),
            ZAddError::Io(e) => write!(f, "{e}"),
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

    #[test]
    fn set_basic_and_uniqueness() {
        let s = Store::new();
        assert!(s.sadd("tags", b("rust")).unwrap()); // new
        assert!(!s.sadd("tags", b("rust")).unwrap()); // duplicate: no-op
        assert!(s.sadd("tags", b("napi")).unwrap());
        assert_eq!(s.scard("tags"), Ok(2));
        assert!(s.sismember("tags", &b("rust")).unwrap());
        assert!(!s.sismember("tags", &b("ghost")).unwrap());
        assert!(!s.sismember("missing", &b("x")).unwrap());

        let mut ms = s.smembers("tags").unwrap();
        ms.sort();
        assert_eq!(ms, vec![b("napi"), b("rust")]);
    }

    #[test]
    fn set_srem_and_empty_key_removed() {
        let s = Store::new();
        s.sadd("s", b("a")).unwrap();
        s.sadd("s", b("b")).unwrap();
        assert!(s.srem("s", &b("a")).unwrap());
        assert!(!s.srem("s", &b("a")).unwrap()); // already gone
        assert_eq!(s.scard("s"), Ok(1));
        assert!(s.srem("s", &b("b")).unwrap());
        assert!(!s.exists("s")); // last member gone -> key deleted
        assert_eq!(s.scard("s"), Ok(0));
        assert!(s.smembers("s").unwrap().is_empty());
        assert!(!s.srem("missing", &b("x")).unwrap());
    }

    #[test]
    fn set_wrong_type() {
        let s = Store::new();
        s.set("str".into(), b("hola"), None).unwrap();
        assert!(matches!(s.sadd("str", b("x")), Err(ListError::WrongType)));
        assert!(matches!(s.srem("str", &b("x")), Err(ListError::WrongType)));
        assert_eq!(s.sismember("str", &b("x")), Err(WrongType));
        assert_eq!(s.smembers("str"), Err(WrongType));
        assert_eq!(s.scard("str"), Err(WrongType));

        // a set is not bytes/list/hash
        s.sadd("st", b("m")).unwrap();
        assert_eq!(s.get("st"), Err(WrongType));
        assert!(matches!(
            s.push_back("st", b("x")),
            Err(ListError::WrongType)
        ));
        assert!(matches!(
            s.hset("st", "f".into(), b("v")),
            Err(ListError::WrongType)
        ));
    }

    #[test]
    fn set_honors_ttl_and_snapshot() {
        let s = Store::new();
        s.sadd("online", b("u1")).unwrap();
        s.sadd("online", b("u2")).unwrap();
        assert!(s.expire("online", 60_000).unwrap());
        let bytes = s.dump_bytes();

        let fresh = Store::new();
        fresh.load_bytes(&bytes).unwrap();
        assert_eq!(fresh.scard("online"), Ok(2));
        assert!(fresh.sismember("online", &b("u1")).unwrap());
        assert!(fresh.ttl("online") > 0);
    }

    #[test]
    fn set_aof_replay_and_compaction() {
        let tmp = Tmp::new("set-aof");
        {
            let (s, _) = Store::with_aof(tmp.path(), false).unwrap();
            s.sadd("seen", b("a")).unwrap();
            s.sadd("seen", b("b")).unwrap();
            s.srem("seen", &b("a")).unwrap();
            // churn: re-adding the same member grows the log without growing the set
            for _ in 0..100 {
                s.sadd("seen", b("b")).unwrap();
            }
            s.sadd("seen", b("c")).unwrap();
            s.close().unwrap();
        }

        let (s2, rec) = Store::with_aof(tmp.path(), false).unwrap();
        assert!(!rec.truncated);
        assert!(!s2.sismember("seen", &b("a")).unwrap()); // removed stays removed
        assert!(s2.sismember("seen", &b("b")).unwrap());
        assert_eq!(s2.scard("seen"), Ok(2)); // {b, c}

        let before = s2.aof_size();
        s2.compact().unwrap();
        assert!(s2.aof_size() < before);
        s2.close().unwrap();

        let (s3, _) = Store::with_aof(tmp.path(), false).unwrap();
        assert_eq!(s3.scard("seen"), Ok(2));
        assert!(s3.sismember("seen", &b("c")).unwrap());
        s3.close().unwrap();
    }

    #[test]
    fn zset_ordering_and_queries() {
        let s = Store::new();
        assert!(matches!(s.zadd("lb", 50.0, b("bob")), Ok(true)));
        assert!(matches!(s.zadd("lb", 100.0, b("alice")), Ok(true)));
        assert!(matches!(s.zadd("lb", 10.0, b("carol")), Ok(true)));
        assert!(matches!(s.zadd("lb", 75.0, b("bob")), Ok(false))); // update

        assert_eq!(s.zcard("lb"), Ok(3));
        assert_eq!(s.zscore("lb", &b("bob")).unwrap(), Some(75.0));
        assert_eq!(s.zscore("lb", &b("ghost")).unwrap(), None);
        assert_eq!(
            s.zrange("lb", 0, -1),
            Ok(vec![b("carol"), b("bob"), b("alice")])
        );
        assert_eq!(s.zrank("lb", &b("carol")).unwrap(), Some(0));
        assert_eq!(s.zrank("lb", &b("alice")).unwrap(), Some(2));

        let top = s.zrange_scored("lb", -1, -1).unwrap();
        assert_eq!(top[0].member, b("alice"));
        assert_eq!(top[0].score, 100.0);
    }

    #[test]
    fn zset_incrby_and_rem() {
        let s = Store::new();
        assert_eq!(s.zincrby("lb", 5.0, b("p")).unwrap(), 5.0); // created at delta
        assert_eq!(s.zincrby("lb", 3.0, b("p")).unwrap(), 8.0);
        assert!(s.zrem("lb", &b("p")).unwrap());
        assert!(!s.zrem("lb", &b("p")).unwrap());
        assert!(!s.exists("lb")); // emptied -> removed
        assert_eq!(s.zcard("lb"), Ok(0));
        assert_eq!(s.zrange("lb", 0, -1), Ok(Vec::new()));
    }

    #[test]
    fn zset_rejects_nan() {
        let s = Store::new();
        assert!(matches!(
            s.zadd("z", f64::NAN, b("x")),
            Err(ZAddError::BadScore)
        ));
    }

    #[test]
    fn zset_wrong_type() {
        let s = Store::new();
        s.set("str".into(), b("hola"), None).unwrap();
        assert!(matches!(
            s.zadd("str", 1.0, b("x")),
            Err(ZAddError::WrongType)
        ));
        assert!(matches!(
            s.zincrby("str", 1.0, b("x")),
            Err(ZAddError::WrongType)
        ));
        assert!(matches!(s.zrem("str", &b("x")), Err(ListError::WrongType)));
        assert_eq!(s.zscore("str", &b("x")), Err(WrongType));
        assert_eq!(s.zrank("str", &b("x")), Err(WrongType));
        assert_eq!(s.zcard("str"), Err(WrongType));
        assert_eq!(s.zrange("str", 0, -1), Err(WrongType));
        assert_eq!(s.zrange_scored("str", 0, -1), Err(WrongType));

        s.zadd("z", 1.0, b("m")).unwrap();
        assert_eq!(s.get("z"), Err(WrongType));
    }

    #[test]
    fn zset_snapshot_and_aof() {
        let tmp = Tmp::new("zset-aof");
        {
            let (s, _) = Store::with_aof(tmp.path(), false).unwrap();
            s.zadd("lb", 100.0, b("alice")).unwrap();
            s.zadd("lb", 50.0, b("bob")).unwrap();
            for _ in 0..100 {
                s.zincrby("lb", 1.0, b("bob")).unwrap(); // churn: 50 -> 150
            }
            s.expire("lb", 60_000).unwrap();
            s.close().unwrap();
        }

        let (s2, rec) = Store::with_aof(tmp.path(), false).unwrap();
        assert!(!rec.truncated);
        assert_eq!(s2.zscore("lb", &b("bob")).unwrap(), Some(150.0));
        assert_eq!(s2.zrange("lb", 0, -1), Ok(vec![b("alice"), b("bob")])); // bob now leads
        assert!(s2.ttl("lb") > 0);

        // snapshot roundtrip preserves scores + order
        let bytes = s2.dump_bytes();
        let fresh = Store::new();
        fresh.load_bytes(&bytes).unwrap();
        assert_eq!(fresh.zscore("lb", &b("bob")).unwrap(), Some(150.0));
        assert_eq!(fresh.zrank("lb", &b("bob")).unwrap(), Some(1));

        let before = s2.aof_size();
        s2.compact().unwrap();
        assert!(s2.aof_size() < before);
        s2.close().unwrap();
    }

    #[test]
    fn tx_commit_applies_all() {
        let s = Store::new();
        s.set("balance".into(), b("100"), None).unwrap();
        s.tx_begin().unwrap();
        assert!(s.in_transaction());
        s.set("balance".into(), b("90"), None).unwrap();
        s.hset("orders", "o1".into(), b("book")).unwrap();
        s.tx_commit().unwrap();
        assert!(!s.in_transaction());
        assert_eq!(s.get("balance"), Ok(Some(b("90"))));
        assert_eq!(s.hget("orders", "o1").unwrap(), Some(b("book")));
    }

    #[test]
    fn tx_rollback_restores_state() {
        let s = Store::new();
        s.set("balance".into(), b("100"), None).unwrap();
        s.hset("h", "keep".into(), b("v")).unwrap();

        s.tx_begin().unwrap();
        s.set("balance".into(), b("0"), None).unwrap(); // change existing
        s.set("new".into(), b("x"), None).unwrap(); // add new
        s.hdel("h", "keep").unwrap(); // remove field
        s.zadd("lb", 5.0, b("p")).unwrap(); // new structure
        s.tx_rollback().unwrap();

        // everything reverts to the pre-transaction state
        assert_eq!(s.get("balance"), Ok(Some(b("100"))));
        assert_eq!(s.get("new"), Ok(None));
        assert_eq!(s.hget("h", "keep").unwrap(), Some(b("v")));
        assert_eq!(s.zcard("lb"), Ok(0));
        assert!(!s.in_transaction());
    }

    #[test]
    fn tx_no_nesting_and_no_stray_commit() {
        let s = Store::new();
        s.tx_begin().unwrap();
        assert!(s.tx_begin().is_err()); // already open
        s.tx_commit().unwrap();
        assert!(s.tx_commit().is_err()); // none open
        assert!(s.tx_rollback().is_err()); // none open
    }

    #[test]
    fn tx_transact_helper_commits_and_rolls_back() {
        let s = Store::new();
        s.set("k".into(), b("1"), None).unwrap();

        // Ok closure commits.
        let out: Result<i32, TxError<()>> = s.transact(|st| {
            st.set("k".into(), b("2"), None).unwrap();
            Ok(42)
        });
        assert_eq!(out.map_err(|_| ()), Ok(42));
        assert_eq!(s.get("k"), Ok(Some(b("2"))));

        // Err closure rolls back.
        let out: Result<(), &str> = match s.transact(|st| {
            st.set("k".into(), b("3"), None).unwrap();
            Err("boom")
        }) {
            Ok(v) => Ok(v),
            Err(TxError::User(e)) => Err(e),
            Err(TxError::Io(_)) => Err("io"),
        };
        assert_eq!(out, Err("boom"));
        assert_eq!(s.get("k"), Ok(Some(b("2")))); // unchanged by the failed tx
    }

    #[test]
    fn tx_persists_atomically_to_aof() {
        let tmp = Tmp::new("tx-aof");
        {
            let (s, _) = Store::with_aof(tmp.path(), false).unwrap();
            s.set("a".into(), b("1"), None).unwrap();
            s.tx_begin().unwrap();
            s.set("a".into(), b("2"), None).unwrap();
            s.set("b".into(), b("3"), None).unwrap();
            s.tx_commit().unwrap();
            // a rolled-back tx writes nothing
            s.tx_begin().unwrap();
            s.set("c".into(), b("never"), None).unwrap();
            s.tx_rollback().unwrap();
            s.close().unwrap();
        }
        let (s2, rec) = Store::with_aof(tmp.path(), false).unwrap();
        assert!(!rec.truncated);
        assert_eq!(s2.get("a"), Ok(Some(b("2"))));
        assert_eq!(s2.get("b"), Ok(Some(b("3"))));
        assert_eq!(s2.get("c"), Ok(None)); // rolled back, never journalled
        s2.close().unwrap();
    }

    // ── Hardening: adversarial and edge-case inputs ───────────────────────

    #[test]
    fn snapshot_with_huge_count_does_not_oom() {
        // A corrupt count field must not trigger a giant allocation. Build a
        // minimal v2 header claiming 4 billion entries with no body to back it.
        let mut data = Vec::new();
        data.extend_from_slice(MAGIC);
        data.push(2); // version 2 (no trailing CRC to worry about)
        data.push(0); // flags
        data.extend_from_slice(&u32::MAX.to_le_bytes()); // count = 4 billion
                                                         // no entries follow
        let s = Store::new();
        // Must return an error (truncated), not abort the process on allocation.
        assert!(s.load_bytes(&data).is_err());
    }

    #[test]
    fn snapshot_with_huge_value_len_is_rejected() {
        // v2 entry whose value-length claims to be enormous but has no bytes.
        let mut data = Vec::new();
        data.extend_from_slice(MAGIC);
        data.push(2);
        data.push(0);
        data.extend_from_slice(&1u32.to_le_bytes()); // count = 1
        data.extend_from_slice(&1u32.to_le_bytes()); // key len = 1
        data.push(b'k'); // key
        data.extend_from_slice(&0u64.to_le_bytes()); // no expiry
        data.push(TYPE_BYTES);
        data.extend_from_slice(&u32::MAX.to_le_bytes()); // value len = 4GB
                                                         // no value bytes
        let s = Store::new();
        assert!(s.load_bytes(&data).is_err()); // truncated, not OOM
    }

    #[test]
    fn snapshot_with_huge_list_count_is_rejected() {
        let mut data = Vec::new();
        data.extend_from_slice(MAGIC);
        data.push(2);
        data.push(0);
        data.extend_from_slice(&1u32.to_le_bytes()); // count = 1
        data.extend_from_slice(&1u32.to_le_bytes()); // key len
        data.push(b'q');
        data.extend_from_slice(&0u64.to_le_bytes());
        data.push(TYPE_LIST);
        data.extend_from_slice(&u32::MAX.to_le_bytes()); // element count = 4B
        let s = Store::new();
        assert!(s.load_bytes(&data).is_err());
    }

    #[test]
    fn keys_and_values_with_arbitrary_bytes() {
        let s = Store::new();
        // NUL bytes, high bytes, and an empty value are all valid.
        let weird_key = "clé\0\u{1F600}".to_string();
        let weird_val = vec![0u8, 255, 0, 128, 42];
        s.set(weird_key.clone(), weird_val.clone(), None).unwrap();
        assert_eq!(s.get(&weird_key), Ok(Some(weird_val)));

        s.set("empty".into(), Vec::new(), None).unwrap();
        assert_eq!(s.get("empty"), Ok(Some(Vec::new()))); // empty value != missing
        assert!(s.exists("empty"));

        // Empty key is a valid key.
        s.set(String::new(), vec![1], None).unwrap();
        assert_eq!(s.get(""), Ok(Some(vec![1])));
    }

    #[test]
    fn arbitrary_bytes_survive_snapshot_roundtrip() {
        let s = Store::new();
        let val: Vec<u8> = (0..=255u8).collect(); // every byte value
        s.set("all-bytes".into(), val.clone(), None).unwrap();
        s.push_back("list", vec![0, 255, 0]).unwrap();
        s.hset("h", "\0field".into(), vec![255, 254]).unwrap();
        s.sadd("set", vec![0, 0, 0]).unwrap();

        let bytes = s.dump_bytes();
        let fresh = Store::new();
        fresh.load_bytes(&bytes).unwrap();
        assert_eq!(fresh.get("all-bytes"), Ok(Some(val)));
        assert_eq!(fresh.lrange("list", 0, -1), Ok(vec![vec![0, 255, 0]]));
        assert_eq!(fresh.hget("h", "\0field").unwrap(), Some(vec![255, 254]));
        assert!(fresh.sismember("set", &[0, 0, 0]).unwrap());
    }

    #[test]
    fn large_value_roundtrips() {
        // A megabyte value should store, fetch, and snapshot without issue.
        let s = Store::new();
        let big = vec![7u8; 1024 * 1024];
        s.set("big".into(), big.clone(), None).unwrap();
        assert_eq!(s.get("big"), Ok(Some(big.clone())));
        let snap = s.dump_bytes();
        let fresh = Store::new();
        fresh.load_bytes(&snap).unwrap();
        assert_eq!(fresh.get("big"), Ok(Some(big)));
    }

    #[test]
    fn truncated_snapshot_headers_are_rejected_cleanly() {
        let s = Store::new();
        // Every prefix of a valid snapshot must error, never panic.
        s.set("k".into(), b("value"), None).unwrap();
        let full = s.dump_bytes();
        for cut in 0..full.len() {
            let fresh = Store::new();
            // Must not panic on any truncation point.
            let _ = fresh.load_bytes(&full[..cut]);
        }
        // And the full thing still loads.
        let fresh = Store::new();
        assert!(fresh.load_bytes(&full).is_ok());
    }

    #[test]
    fn lrange_extreme_indices_never_panic() {
        let s = Store::new();
        for c in ["a", "b", "c"] {
            s.push_back("l", b(c)).unwrap();
        }
        // i64 extremes must not overflow the index math.
        assert_eq!(
            s.lrange("l", i64::MIN, i64::MAX),
            Ok(vec![b("a"), b("b"), b("c")])
        );
        assert_eq!(s.lrange("l", i64::MAX, i64::MIN), Ok(Vec::new()));
        assert_eq!(s.lrange("l", -1000, 1000), Ok(vec![b("a"), b("b"), b("c")]));
    }

    #[test]
    fn zrange_extreme_indices_never_panic() {
        let s = Store::new();
        s.zadd("z", 1.0, b("a")).unwrap();
        s.zadd("z", 2.0, b("b")).unwrap();
        assert_eq!(s.zrange("z", i64::MIN, i64::MAX), Ok(vec![b("a"), b("b")]));
        assert_eq!(s.zrange("z", i64::MAX, i64::MIN), Ok(Vec::new()));
    }

    #[test]
    fn tx_undo_same_key_touched_many_times() {
        // The undo-log must record only the ORIGINAL value, no matter how many
        // times a key changes within the transaction.
        let s = Store::new();
        s.set("k".into(), b("original"), None).unwrap();
        s.tx_begin().unwrap();
        s.set("k".into(), b("v1"), None).unwrap();
        s.set("k".into(), b("v2"), None).unwrap();
        s.set("k".into(), b("v3"), None).unwrap();
        s.tx_rollback().unwrap();
        assert_eq!(s.get("k"), Ok(Some(b("original")))); // back to original, not v2
    }

    #[test]
    fn tx_rollback_of_clear_restores_everything() {
        let s = Store::new();
        s.set("a".into(), b("1"), None).unwrap();
        s.set("b".into(), b("2"), None).unwrap();
        s.hset("h", "f".into(), b("v")).unwrap();
        s.tx_begin().unwrap();
        s.clear().unwrap();
        assert_eq!(s.size(), 0); // cleared inside the tx
        s.tx_rollback().unwrap();
        // everything comes back
        assert_eq!(s.get("a"), Ok(Some(b("1"))));
        assert_eq!(s.get("b"), Ok(Some(b("2"))));
        assert_eq!(s.hget("h", "f").unwrap(), Some(b("v")));
    }

    #[test]
    fn tx_rollback_across_all_structures() {
        let s = Store::new();
        s.set("kv".into(), b("keep"), None).unwrap();
        s.push_back("list", b("keep")).unwrap();
        s.hset("hash", "keep".into(), b("v")).unwrap();
        s.sadd("set", b("keep")).unwrap();
        s.zadd("zset", 1.0, b("keep")).unwrap();

        s.tx_begin().unwrap();
        s.set("kv".into(), b("changed"), None).unwrap();
        s.push_back("list", b("added")).unwrap();
        s.hset("hash", "new".into(), b("x")).unwrap();
        s.sadd("set", b("added")).unwrap();
        s.zadd("zset", 2.0, b("added")).unwrap();
        s.set("brand-new".into(), b("x"), None).unwrap();
        s.tx_rollback().unwrap();

        // every structure is back exactly as before the transaction
        assert_eq!(s.get("kv"), Ok(Some(b("keep"))));
        assert_eq!(s.lrange("list", 0, -1), Ok(vec![b("keep")]));
        assert_eq!(s.hlen("hash"), Ok(1));
        assert_eq!(s.hget("hash", "keep").unwrap(), Some(b("v")));
        assert_eq!(s.scard("set"), Ok(1));
        assert_eq!(s.zcard("zset"), Ok(1));
        assert_eq!(s.get("brand-new"), Ok(None)); // never existed before
    }

    #[test]
    fn tx_commit_then_rollback_noop_is_safe() {
        // A key created and committed, then a later tx that rolls back, must keep
        // the committed value.
        let s = Store::new();
        s.tx_begin().unwrap();
        s.set("k".into(), b("committed"), None).unwrap();
        s.tx_commit().unwrap();

        s.tx_begin().unwrap();
        s.set("k".into(), b("temp"), None).unwrap();
        s.tx_rollback().unwrap();
        assert_eq!(s.get("k"), Ok(Some(b("committed"))));
    }
}
