//! Streaming JSONL snapshot writer (`--record`) and parser (`--replay`).

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};

use thiserror::Error;

use crate::Snapshot;

const RECORDER_CHANNEL_CAPACITY: usize = 64;

#[derive(Debug, Error)]
pub enum JsonlError {
    #[error("I/O error while accessing snapshot stream: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed JSONL snapshot on line {line}: {source}")]
    Parse {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("snapshot JSONL stream contains no snapshots")]
    EmptyStream,
}

/// Summary counts returned when a [`SnapshotRecorder`] finishes flushing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordStats {
    pub written: u64,
    pub dropped: u64,
}

/// Serializes one [`Snapshot`] as a single JSONL line and flushes it.
pub fn write_snapshot_line<W: Write>(
    writer: &mut W,
    snapshot: &Snapshot,
) -> Result<(), JsonlError> {
    let line = serde_json::to_string(snapshot)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Parses all non-blank JSONL lines from `reader` into [`Snapshot`] records.
///
/// Malformed lines return [`JsonlError::Parse`] with their 1-based line number.
pub fn parse_jsonl_reader<R: BufRead>(reader: R) -> Result<Vec<Snapshot>, JsonlError> {
    let mut snapshots = Vec::new();
    for (idx, line_result) in reader.lines().enumerate() {
        let line_no = idx + 1;
        let line = line_result?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let snapshot: Snapshot =
            serde_json::from_str(trimmed).map_err(|source| JsonlError::Parse {
                line: line_no,
                source,
            })?;
        snapshots.push(snapshot);
    }
    if snapshots.is_empty() {
        return Err(JsonlError::EmptyStream);
    }
    Ok(snapshots)
}

/// Opens and parses a JSONL snapshot file from disk.
pub fn parse_jsonl_file(path: &Path) -> Result<Vec<Snapshot>, JsonlError> {
    let file = File::open(path)?;
    parse_jsonl_reader(BufReader::new(file))
}

/// Non-blocking JSONL snapshot recorder backed by a bounded channel and a
/// dedicated writer thread so disk I/O never stalls the UI render loop.
pub struct SnapshotRecorder {
    tx: Option<SyncSender<Snapshot>>,
    handle: Option<JoinHandle<Result<u64, JsonlError>>>,
    dropped: Arc<AtomicU64>,
}

impl fmt::Debug for SnapshotRecorder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotRecorder")
            .field("active", &self.tx.is_some())
            .field("dropped", &self.dropped.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl SnapshotRecorder {
    /// Opens `path` in create-and-append mode and starts the background writer thread.
    ///
    /// If `path` already exists with content that does not end in a newline, a newline
    /// separator is written before the first appended snapshot so existing JSONL records
    /// remain intact.
    pub fn open(path: &Path) -> Result<Self, JsonlError> {
        let mut opts = OpenOptions::new();
        opts.read(true).create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            // Pass `O_NOFOLLOW` directly to `open(2)` so symlink replacement cannot redirect
            // snapshot recordings (zero TOCTOU between path check and open).
            opts.mode(0o600);
            #[allow(clippy::cast_possible_wrap)]
            let nofollow = rustix::fs::OFlags::NOFOLLOW.bits() as i32;
            opts.custom_flags(nofollow);
        }
        let mut file = opts.open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
            // Verify the already-open file descriptor (`fstat`) is a regular file owned by our
            // effective UID with owner-only permissions (`0600` or stricter) before writing any
            // bytes containing process command lines.
            let meta = file.metadata()?;
            let current_euid = rustix::process::geteuid().as_raw();
            if !meta.file_type().is_file() {
                return Err(JsonlError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "refusing to write snapshot recording to a non-regular file",
                )));
            }
            if meta.uid() != current_euid {
                return Err(JsonlError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "refusing to append snapshot recording to a file owned by another user",
                )));
            }
            if (meta.permissions().mode() & 0o077) != 0 {
                return Err(JsonlError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "refusing to append snapshot recording to a group- or world-accessible file (expected mode 0600)",
                )));
            }
        }
        let mut needs_leading_newline = false;
        if file.metadata()?.len() > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0u8; 1];
            file.read_exact(&mut last)?;
            file.seek(SeekFrom::End(0))?;
            needs_leading_newline = last[0] != b'\n';
        }

        let (tx, rx) = mpsc::sync_channel::<Snapshot>(RECORDER_CHANNEL_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));

        let handle = thread::Builder::new()
            .name("btop-jsonl-recorder".to_owned())
            .spawn(move || {
                let mut writer = BufWriter::new(file);
                let mut written = 0u64;
                while let Ok(snapshot) = rx.recv() {
                    if needs_leading_newline {
                        writer.write_all(b"\n")?;
                        needs_leading_newline = false;
                    }
                    write_snapshot_line(&mut writer, &snapshot)?;
                    written += 1;
                }
                writer.flush()?;
                Ok(written)
            })?;

        Ok(Self {
            tx: Some(tx),
            handle: Some(handle),
            dropped,
        })
    }

    /// Enqueues a snapshot for background JSONL serialization without blocking the caller.
    pub fn record(&self, snapshot: Snapshot) {
        let Some(tx) = self.tx.as_ref() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        match tx.try_send(snapshot) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Closes the channel, waits for the writer thread to flush, and returns stats.
    pub fn finish(mut self) -> Result<RecordStats, JsonlError> {
        self.tx.take();
        let written = if let Some(handle) = self.handle.take() {
            match handle.join() {
                Ok(res) => res?,
                Err(_) => {
                    return Err(JsonlError::Io(std::io::Error::other(
                        "snapshot recorder thread panicked",
                    )));
                }
            }
        } else {
            0
        };
        Ok(RecordStats {
            written,
            dropped: self.dropped.load(Ordering::Relaxed),
        })
    }
}

impl Drop for SnapshotRecorder {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::fixtures::win11_8core_single_gpu_desktop;

    #[test]
    fn malformed_jsonl_line_returns_parse_error_with_line_number() {
        let snap = &win11_8core_single_gpu_desktop()[0];
        let valid_line = serde_json::to_string(snap).expect("serialize");
        let stream = format!("{valid_line}\n{{not valid json\n");

        let err =
            parse_jsonl_reader(Cursor::new(stream)).expect_err("malformed line must return error");
        match err {
            JsonlError::Parse { line, .. } => assert_eq!(line, 2),
            other => panic!("expected JsonlError::Parse, got {other:?}"),
        }
    }

    #[test]
    fn empty_or_whitespace_stream_returns_empty_stream_error() {
        let err = parse_jsonl_reader(Cursor::new("   \n\n\t\n"))
            .expect_err("empty stream must return error");
        assert!(matches!(err, JsonlError::EmptyStream));
    }

    #[test]
    fn recorder_appends_and_roundtrips_snapshots() {
        let unique = format!(
            "btop-windows-recorder-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let path = std::env::temp_dir().join(unique);
        let fixtures = win11_8core_single_gpu_desktop();

        let recorder = SnapshotRecorder::open(&path).expect("open recorder");
        for snap in &fixtures {
            recorder.record(snap.clone());
        }
        let stats = recorder.finish().expect("finish recorder");
        assert_eq!(stats.written, fixtures.len() as u64);
        assert_eq!(stats.dropped, 0);

        let loaded = parse_jsonl_file(&path).expect("reload recorded file");
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded, fixtures);
    }

    #[test]
    fn recorder_preserves_boundary_when_appending_to_file_without_trailing_newline() {
        let unique = format!(
            "btop-windows-boundary-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let path = std::env::temp_dir().join(unique);
        let fixtures = win11_8core_single_gpu_desktop();

        // Write tick 0 without a trailing newline (owner-only mode on Unix so append is permitted).
        let first_line = serde_json::to_string(&fixtures[0]).expect("serialize tick 0");
        std::fs::write(&path, first_line.as_bytes()).expect("seed file without trailing newline");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("set owner-only permissions on seeded recording");
        }

        let recorder = SnapshotRecorder::open(&path).expect("open recorder on existing file");
        recorder.record(fixtures[1].clone());
        recorder.record(fixtures[2].clone());
        let stats = recorder.finish().expect("finish recorder");
        assert_eq!(stats.written, 2);
        assert_eq!(stats.dropped, 0);

        let loaded = parse_jsonl_file(&path).expect("reloaded stream must stay valid");
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded, fixtures);
    }

    #[cfg(unix)]
    #[test]
    fn recorder_restricts_unix_file_mode_to_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let unique = format!(
            "btop-windows-mode-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let path = std::env::temp_dir().join(unique);
        let fixtures = win11_8core_single_gpu_desktop();

        // Newly created recording gets mode 0600.
        let recorder = SnapshotRecorder::open(&path).expect("open recorder");
        recorder.record(fixtures[0].clone());
        let _ = recorder.finish().expect("finish recorder");

        let mode = std::fs::metadata(&path)
            .expect("stat recording")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        // Appending to an already-owner-only (0600) file succeeds cleanly.
        let append_ok = SnapshotRecorder::open(&path).expect("append to 0600 recording");
        append_ok.record(fixtures[1].clone());
        let _ = append_ok.finish().expect("finish append");

        // Widening the file to 0644 must cause `open` to fail closed without appending.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("set 0644 permissions");
        let len_before = std::fs::metadata(&path).expect("stat before").len();
        let err = SnapshotRecorder::open(&path)
            .expect_err("must refuse appending to world-readable recording");
        assert!(matches!(
            err,
            JsonlError::Io(ref io_err) if io_err.kind() == std::io::ErrorKind::PermissionDenied
        ));
        assert_eq!(
            std::fs::metadata(&path).expect("stat after").len(),
            len_before
        );

        // Symlink target must be rejected atomically by `O_NOFOLLOW`.
        let link_path = path.with_extension("symlink.jsonl");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("restore 0600");
        std::os::unix::fs::symlink(&path, &link_path).expect("create symlink");
        assert!(
            SnapshotRecorder::open(&link_path).is_err(),
            "O_NOFOLLOW must reject symlink recording target"
        );
        let _ = std::fs::remove_file(&link_path);
        let _ = std::fs::remove_file(&path);
    }
}
