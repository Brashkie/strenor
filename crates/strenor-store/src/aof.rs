//! Append-only log (AOF): durability for the in-memory store.
//!
//! Every mutation is appended as a self-checked record, so a crash loses at most
//! the writes that never reached the file. On startup the log is replayed to
//! rebuild state.
//!
//! Record framing (little-endian):
//!   `[len:u32][crc32:u32][payload: len bytes]`
//!
//! The CRC covers the payload only. A record whose length runs past EOF or whose
//! CRC doesn't match is treated as a **torn tail**: replay stops there and the
//! file is truncated to the last good record. That is exactly the crash case —
//! the process died mid-append — and it must not be a fatal error.
//!
//! Payload layout: `[opcode:u8][args...]`, mirroring the store's mutations.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

pub(crate) const OP_SET: u8 = 1;
pub(crate) const OP_DEL: u8 = 2;
pub(crate) const OP_EXPIRE: u8 = 3;
pub(crate) const OP_PERSIST: u8 = 4;
pub(crate) const OP_CLEAR: u8 = 5;
pub(crate) const OP_PUSH_FRONT: u8 = 6;
pub(crate) const OP_PUSH_BACK: u8 = 7;
pub(crate) const OP_POP_FRONT: u8 = 8;
pub(crate) const OP_POP_BACK: u8 = 9;

/// CRC-32 (IEEE), table-driven. Built at compile time — no dependency, no
/// runtime init. Used for error detection only; this is not a hash for security.
const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        let idx = ((crc ^ byte as u32) & 0xff) as usize;
        crc = (crc >> 8) ^ CRC_TABLE[idx];
    }
    !crc
}

// ── Record encoding ───────────────────────────────────────────────────────

fn put_bytes(buf: &mut Vec<u8>, b: &[u8]) {
    buf.extend_from_slice(&(b.len() as u32).to_le_bytes());
    buf.extend_from_slice(b);
}

pub(crate) fn rec_set(key: &str, value: &[u8], expire_at: Option<u64>) -> Vec<u8> {
    let mut p = vec![OP_SET];
    put_bytes(&mut p, key.as_bytes());
    p.extend_from_slice(&expire_at.unwrap_or(0).to_le_bytes());
    put_bytes(&mut p, value);
    p
}

pub(crate) fn rec_key_only(op: u8, key: &str) -> Vec<u8> {
    let mut p = vec![op];
    put_bytes(&mut p, key.as_bytes());
    p
}

pub(crate) fn rec_expire(key: &str, expire_at: u64) -> Vec<u8> {
    let mut p = vec![OP_EXPIRE];
    put_bytes(&mut p, key.as_bytes());
    p.extend_from_slice(&expire_at.to_le_bytes());
    p
}

pub(crate) fn rec_push(op: u8, key: &str, value: &[u8]) -> Vec<u8> {
    let mut p = vec![op];
    put_bytes(&mut p, key.as_bytes());
    put_bytes(&mut p, value);
    p
}

pub(crate) fn rec_clear() -> Vec<u8> {
    vec![OP_CLEAR]
}

/// Cursor over a decoded payload, used during replay.
pub(crate) struct Cursor<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Cursor { data, pos: 1 } // skip opcode
    }
    pub fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = self.u32()? as usize;
        if self.pos + len > self.data.len() {
            return None;
        }
        let out = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Some(out)
    }
    pub fn string(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }
    pub fn u32(&mut self) -> Option<u32> {
        if self.pos + 4 > self.data.len() {
            return None;
        }
        let v = u32::from_le_bytes(self.data[self.pos..self.pos + 4].try_into().ok()?);
        self.pos += 4;
        Some(v)
    }
    pub fn u64(&mut self) -> Option<u64> {
        if self.pos + 8 > self.data.len() {
            return None;
        }
        let v = u64::from_le_bytes(self.data[self.pos..self.pos + 8].try_into().ok()?);
        self.pos += 8;
        Some(v)
    }
}

// ── Writer ────────────────────────────────────────────────────────────────

/// Owns the log file handle. Appends are flushed to the OS on every mutation, so
/// a *process* crash loses nothing; `fsync = true` additionally forces the data
/// to disk on every write, surviving an OS/power loss at a large speed cost.
pub(crate) struct Aof {
    path: PathBuf,
    /// `None` once closed. Windows refuses to delete or replace a file that
    /// still has an open handle, so releasing this is part of the contract —
    /// not just tidiness.
    file: Option<BufWriter<File>>,
    fsync: bool,
}

impl Aof {
    /// Open (creating if needed) for appending.
    pub fn open(path: &Path, fsync: bool) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Aof {
            path: path.to_path_buf(),
            file: Some(BufWriter::new(file)),
            fsync,
        })
    }

    fn handle(&mut self) -> io::Result<&mut BufWriter<File>> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("log is closed"))
    }

    /// Flush and release the file handle. After this the log is untouchable by
    /// this process, so the file can be deleted, moved, or reopened.
    pub fn close(&mut self) -> io::Result<()> {
        if let Some(mut f) = self.file.take() {
            f.flush()?;
        }
        Ok(())
    }

    pub fn append(&mut self, payload: &[u8]) -> io::Result<()> {
        let mut frame = Vec::with_capacity(payload.len() + 8);
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&crc32(payload).to_le_bytes());
        frame.extend_from_slice(payload);
        let fsync = self.fsync;
        let file = self.handle()?;
        file.write_all(&frame)?;
        file.flush()?; // reach the OS: survives a process crash
        if fsync {
            file.get_ref().sync_data()?; // reach the disk: survives power loss
        }
        Ok(())
    }

    /// Rewrite the log from scratch with `records` (compaction). Writes to a
    /// temp file and renames, so a crash never leaves a half-written log.
    pub fn rewrite(&mut self, records: &[Vec<u8>]) -> io::Result<()> {
        let tmp = self.path.with_extension("aof.compact");
        {
            let mut out = BufWriter::new(File::create(&tmp)?);
            for payload in records {
                out.write_all(&(payload.len() as u32).to_le_bytes())?;
                out.write_all(&crc32(payload).to_le_bytes())?;
                out.write_all(payload)?;
            }
            out.flush()?;
            out.get_ref().sync_data()?;
        }
        // Let go of our handle first: Windows refuses to replace a file that is
        // still open, and on Unix the old handle would point at the replaced
        // inode anyway. Either way, reopen after the rename.
        self.close()?;
        std::fs::rename(&tmp, &self.path)?;
        self.file = Some(BufWriter::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        ));
        Ok(())
    }

    /// Bytes currently on disk (used to decide when compaction is worthwhile).
    pub fn size(&self) -> u64 {
        std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }
}

/// Read every intact record from `path`.
///
/// Returns the records plus the byte offset of the first damaged one (if any).
/// A torn tail is expected after a crash, so it is reported — not an error.
pub(crate) fn read_records(path: &Path) -> io::Result<(Vec<Vec<u8>>, Option<u64>)> {
    let mut data = Vec::new();
    match File::open(path) {
        Ok(mut f) => {
            f.read_to_end(&mut data)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), None)),
        Err(e) => return Err(e),
    }

    let mut out = Vec::new();
    let mut pos = 0usize;
    loop {
        if pos == data.len() {
            return Ok((out, None)); // clean EOF
        }
        if pos + 8 > data.len() {
            return Ok((out, Some(pos as u64))); // torn header
        }
        let len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let crc = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap());
        let start = pos + 8;
        if start + len > data.len() {
            return Ok((out, Some(pos as u64))); // torn payload
        }
        let payload = &data[start..start + len];
        if crc32(payload) != crc {
            return Ok((out, Some(pos as u64))); // corrupted record
        }
        out.push(payload.to_vec());
        pos = start + len;
    }
}

/// Drop everything from `offset` on — removes a torn tail after a crash.
pub(crate) fn truncate_at(path: &Path, offset: u64) -> io::Result<()> {
    let f = OpenOptions::new().write(true).open(path)?;
    f.set_len(offset)?;
    f.sync_all()?;
    Ok(())
}

/// Write `data` to `path` atomically: temp file, fsync, then rename. Without
/// this, a crash mid-write leaves a truncated snapshot and the data is gone.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.flush()?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_known_vector() {
        // "123456789" -> 0xCBF43926 is the standard CRC-32/IEEE check value.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn record_roundtrip_via_cursor() {
        let r = rec_set("k", b"val", Some(42));
        assert_eq!(r[0], OP_SET);
        let mut c = Cursor::new(&r);
        assert_eq!(c.string().unwrap(), "k");
        assert_eq!(c.u64().unwrap(), 42);
        assert_eq!(c.bytes().unwrap(), b"val");
    }
}
