//! Streaming JSONL snapshot writer (`--record`) and parser (`--replay`).

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
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
    pub fn open(path: &Path) -> Result<Self, JsonlError> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let (tx, rx) = mpsc::sync_channel::<Snapshot>(RECORDER_CHANNEL_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));

        let handle = thread::Builder::new()
            .name("btop-jsonl-recorder".to_owned())
            .spawn(move || {
                let mut writer = BufWriter::new(file);
                let mut written = 0u64;
                while let Ok(snapshot) = rx.recv() {
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
}
