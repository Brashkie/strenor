//! NAPI bindings for Strenor. This crate is a thin FFI wrapper: all real logic
//! lives in the `strenor-store` crate (pure Rust, unit-tested). Here we only
//! convert between JS values (`Buffer`, `String`) and the core, surface
//! `WRONGTYPE` errors, and do the snapshot file I/O.

use napi::bindgen_prelude::Buffer;
use napi::{Error, Result, Status};
use napi_derive::napi;
use strenor_store::{Store, WrongType};

/// Map a structure-type mismatch to a Redis-style error thrown into JS.
fn wrongtype(_: WrongType) -> Error {
    Error::new(
        Status::GenericFailure,
        "WRONGTYPE Operation against a key holding the wrong kind of value",
    )
}

#[napi]
pub struct Strenor {
    store: Store,
}

#[napi]
impl Strenor {
    #[napi(constructor)]
    pub fn new() -> Self {
        Strenor {
            store: Store::new(),
        }
    }

    // ── Bytes (KV) ────────────────────────────────────────────────────────

    #[napi]
    pub fn set(&self, key: String, value: Buffer, ttl_ms: Option<i64>) {
        self.store.set(key, value.to_vec(), ttl_ms);
    }

    #[napi]
    pub fn get(&self, key: String) -> Result<Option<Buffer>> {
        self.store
            .get(&key)
            .map(|o| o.map(Buffer::from))
            .map_err(wrongtype)
    }

    // ── Key management ────────────────────────────────────────────────────

    #[napi]
    pub fn del(&self, key: String) -> bool {
        self.store.del(&key)
    }

    #[napi]
    pub fn exists(&self, key: String) -> bool {
        self.store.exists(&key)
    }

    #[napi]
    pub fn expire(&self, key: String, ttl_ms: i64) -> bool {
        self.store.expire(&key, ttl_ms)
    }

    #[napi]
    pub fn persist(&self, key: String) -> bool {
        self.store.persist(&key)
    }

    #[napi]
    pub fn ttl(&self, key: String) -> i64 {
        self.store.ttl(&key)
    }

    #[napi]
    pub fn keys(&self) -> Vec<String> {
        self.store.keys()
    }

    #[napi]
    pub fn size(&self) -> u32 {
        self.store.size()
    }

    #[napi]
    pub fn clear(&self) {
        self.store.clear();
    }

    #[napi]
    pub fn sweep(&self) -> u32 {
        self.store.sweep()
    }

    // ── List ──────────────────────────────────────────────────────────────

    #[napi]
    pub fn push_front(&self, key: String, value: Buffer) -> Result<u32> {
        self.store
            .push_front(&key, value.to_vec())
            .map_err(wrongtype)
    }

    #[napi]
    pub fn push_back(&self, key: String, value: Buffer) -> Result<u32> {
        self.store
            .push_back(&key, value.to_vec())
            .map_err(wrongtype)
    }

    #[napi]
    pub fn pop_front(&self, key: String) -> Result<Option<Buffer>> {
        self.store
            .pop_front(&key)
            .map(|o| o.map(Buffer::from))
            .map_err(wrongtype)
    }

    #[napi]
    pub fn pop_back(&self, key: String) -> Result<Option<Buffer>> {
        self.store
            .pop_back(&key)
            .map(|o| o.map(Buffer::from))
            .map_err(wrongtype)
    }

    #[napi]
    pub fn llen(&self, key: String) -> Result<u32> {
        self.store.llen(&key).map_err(wrongtype)
    }

    #[napi]
    pub fn lrange(&self, key: String, start: i64, stop: i64) -> Result<Vec<Buffer>> {
        self.store
            .lrange(&key, start, stop)
            .map(|v| v.into_iter().map(Buffer::from).collect())
            .map_err(wrongtype)
    }

    // ── Snapshot ──────────────────────────────────────────────────────────

    #[napi]
    pub fn dump(&self, path: String) -> Result<()> {
        std::fs::write(&path, self.store.dump_bytes())
            .map_err(|err| Error::new(Status::GenericFailure, format!("dump failed: {err}")))
    }

    #[napi]
    pub fn load(&self, path: String) -> Result<()> {
        let data = std::fs::read(&path)
            .map_err(|err| Error::new(Status::GenericFailure, format!("load failed: {err}")))?;
        self.store
            .load_bytes(&data)
            .map_err(|e| Error::new(Status::InvalidArg, e.0))
    }
}

impl Default for Strenor {
    fn default() -> Self {
        Self::new()
    }
}
