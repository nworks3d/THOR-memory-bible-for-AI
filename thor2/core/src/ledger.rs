//! A read-only reader for the OLD (1.0) system's key-value ledger sidecar
//! (schema: `kv(ns TEXT, k TEXT, v TEXT, ts INTEGER)`, primary key `(ns, k)`).
//! 1.0 is frozen (CONTRACT.md's own hard boundary): this module never writes
//! a byte to that file, and opens it `SQLITE_OPEN_READ_ONLY` so even an
//! accidental write attempt fails loudly instead of touching the live
//! system. A caller migrating something out of it should hand this a COPY of
//! the live file, never the original, so a read here can never contend with
//! the running system for even a read lock.

use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// The raw text value stored at `(namespace, key)` in `path`'s `kv` table, or
/// `None` if that exact row does not exist. Any I/O or schema error is
/// surfaced, never swallowed - this reads a real ledger, and folding "could
/// not open the file" into "the row is not there" would misreport a broken
/// path as an empty one.
pub fn read_kv(path: &Path, namespace: &str, key: &str) -> anyhow::Result<Option<String>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare("SELECT v FROM kv WHERE ns = ? AND k = ?")?;
    let mut rows = stmt.query(rusqlite::params![namespace, key])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get::<_, String>(0)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_ledger(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("fixture-ledger.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE kv (
                ns TEXT NOT NULL,
                k  TEXT NOT NULL,
                v  TEXT NOT NULL,
                ts INTEGER NOT NULL,
                PRIMARY KEY (ns, k)
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO kv (ns, k, v, ts) VALUES ('pins', 'list', '[\"a\",\"b\"]', 1)",
            [],
        )
        .unwrap();
        path
    }

    #[test]
    fn reads_back_an_existing_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_ledger(dir.path());
        let v = read_kv(&path, "pins", "list").unwrap();
        assert_eq!(v, Some("[\"a\",\"b\"]".to_string()));
    }

    #[test]
    fn a_missing_key_is_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_ledger(dir.path());
        let v = read_kv(&path, "pins", "no-such-key").unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn a_missing_namespace_is_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_ledger(dir.path());
        let v = read_kv(&path, "no-such-namespace", "list").unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn a_write_attempt_is_refused_by_the_open_flags() {
        // Belt-and-braces on the "1.0 is frozen" boundary: even if a future
        // change to this module accidentally tried to write, the connection
        // itself is opened read-only and would refuse it.
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_ledger(dir.path());
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let err = conn.execute("DELETE FROM kv", []).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.to_lowercase().contains("read-only") || msg.to_lowercase().contains("readonly"),
            "expected a read-only rejection, got: {msg}"
        );
    }

    #[test]
    fn a_nonexistent_file_is_a_surfaced_error_not_a_silent_none() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.db");
        assert!(read_kv(&missing, "pins", "list").is_err());
    }
}
