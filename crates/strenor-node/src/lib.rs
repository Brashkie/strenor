//! NAPI bindings for Strenor. This crate is a thin FFI wrapper: all real logic
//! lives in the `strenor-store` crate (pure Rust, unit-tested). Here we only
//! convert between JS values (`Buffer`, `String`) and the core, surface
//! `WRONGTYPE`/IO errors, and do snapshot file I/O.

use napi::bindgen_prelude::Buffer;
use napi::{Error, Result, Status};
use napi_derive::napi;
use std::path::Path;
use strenor_store::{write_atomic, ListError, Store, WrongType};

const WRONGTYPE_MSG: &str = "WRONGTYPE Operation against a key holding the wrong kind of value";

fn wrongtype(_: WrongType) -> Error {
    Error::new(Status::GenericFailure, WRONGTYPE_MSG)
}

fn io_err(err: std::io::Error) -> Error {
    Error::new(Status::GenericFailure, format!("strenor: {err}"))
}

/// A list op can fail on structure type or on journalling; map both faithfully
/// so the JS side can tell "wrong type" from "the disk is full".
fn list_err(e: ListError) -> Error {
    match e {
        ListError::WrongType => Error::new(Status::GenericFailure, WRONGTYPE_MSG),
        ListError::Io(err) => io_err(err),
    }
}

/// Result of replaying an append-only log at startup.
#[napi(object)]
pub struct RecoveryInfo {
    /// Records applied from the log.
    pub applied: u32,
    /// A torn tail was found and dropped (the process had crashed mid-append).
    pub truncated: bool,
}

#[napi]
pub struct Strenor {
    store: Store,
    recovery: Option<RecoveryInfo>,
}

#[napi]
impl Strenor {
    /// `aof_path` attaches an append-only log (replayed on open). `fsync` forces
    /// every write to disk — durable against power loss, much slower.
    #[napi(constructor)]
    pub fn new(aof_path: Option<String>, fsync: Option<bool>) -> Result<Self> {
        match aof_path {
            Some(p) => {
                let (store, rec) =
                    Store::with_aof(Path::new(&p), fsync.unwrap_or(false)).map_err(io_err)?;
                Ok(Strenor {
                    store,
                    recovery: Some(RecoveryInfo {
                        applied: rec.applied,
                        truncated: rec.truncated,
                    }),
                })
            }
            None => Ok(Strenor {
                store: Store::new(),
                recovery: None,
            }),
        }
    }

    /// Replay stats, or `null` when the store has no log.
    #[napi(getter)]
    pub fn recovery(&self) -> Option<RecoveryInfo> {
        self.recovery.as_ref().map(|r| RecoveryInfo {
            applied: r.applied,
            truncated: r.truncated,
        })
    }

    // ── Bytes (KV) ────────────────────────────────────────────────────────

    #[napi]
    pub fn set(&self, key: String, value: Buffer, ttl_ms: Option<i64>) -> Result<()> {
        self.store.set(key, value.to_vec(), ttl_ms).map_err(io_err)
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
    pub fn del(&self, key: String) -> Result<bool> {
        self.store.del(&key).map_err(io_err)
    }

    #[napi]
    pub fn exists(&self, key: String) -> bool {
        self.store.exists(&key)
    }

    #[napi]
    pub fn expire(&self, key: String, ttl_ms: i64) -> Result<bool> {
        self.store.expire(&key, ttl_ms).map_err(io_err)
    }

    #[napi]
    pub fn persist(&self, key: String) -> Result<bool> {
        self.store.persist(&key).map_err(io_err)
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
    pub fn clear(&self) -> Result<()> {
        self.store.clear().map_err(io_err)
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
            .map_err(list_err)
    }

    #[napi]
    pub fn push_back(&self, key: String, value: Buffer) -> Result<u32> {
        self.store.push_back(&key, value.to_vec()).map_err(list_err)
    }

    #[napi]
    pub fn pop_front(&self, key: String) -> Result<Option<Buffer>> {
        self.store
            .pop_front(&key)
            .map(|o| o.map(Buffer::from))
            .map_err(list_err)
    }

    #[napi]
    pub fn pop_back(&self, key: String) -> Result<Option<Buffer>> {
        self.store
            .pop_back(&key)
            .map(|o| o.map(Buffer::from))
            .map_err(list_err)
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

    // ── AOF ───────────────────────────────────────────────────────────────

    #[napi]
    pub fn has_aof(&self) -> bool {
        self.store.has_aof()
    }

    #[napi]
    pub fn aof_size(&self) -> i64 {
        self.store.aof_size() as i64
    }

    /// Rewrite the log to the shortest form that reproduces current state.
    #[napi]
    pub fn compact(&self) -> Result<i64> {
        self.store.compact().map(|n| n as i64).map_err(io_err)
    }

    /// Flush and release the log's file handle. Idempotent. Call on shutdown:
    /// on Windows the file cannot be deleted or replaced while it stays open.
    #[napi]
    pub fn close(&self) -> Result<()> {
        self.store.close().map_err(io_err)
    }

    // ── Snapshot ──────────────────────────────────────────────────────────

    /// Write a snapshot atomically (temp file + rename), so a crash mid-write
    /// can never truncate an existing snapshot.
    #[napi]
    pub fn dump(&self, path: String) -> Result<()> {
        write_atomic(Path::new(&path), &self.store.dump_bytes()).map_err(io_err)
    }

    #[napi]
    pub fn load(&self, path: String) -> Result<()> {
        let data = std::fs::read(&path).map_err(io_err)?;
        self.store
            .load_bytes(&data)
            .map_err(|e| Error::new(Status::InvalidArg, e.0))
    }
}
