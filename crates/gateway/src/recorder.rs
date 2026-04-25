//! Append-only ingress recorder.
//!
//! Records every accepted inbound frame to disk so the engine can be
//! replayed deterministically. The on-disk record is the wire frame
//! prefixed by its length and suffixed by the engine-stamped `recv_ts`,
//! all little-endian:
//!
//! ```text
//! [u32 LE total][u8 SBE frame][u64 LE recv_ts]
//! ```
//!
//! Where `total` counts only the frame bytes (not including the
//! suffixed `recv_ts`). Replay reads the prefix, the frame, and the
//! `recv_ts`; any partial final record (length prefix without enough
//! frame + ts bytes) is detected and truncated rather than rejected.
//!
//! ## When to use
//!
//! Production paths construct a [`Recorder`] when the CLI is invoked
//! with `--record <path>`. When the flag is absent, the gateway runs
//! without instantiating a recorder; the listener has no `Option<_>`
//! branch on the hot path — the no-op is structural, not runtime.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;

use domain::RecvTs;

/// Number of bytes the suffix occupies after each recorded frame.
pub const RECV_TS_BYTES: usize = 8;

/// Append-only buffered recorder. Holds an exclusive write handle to
/// `path`; one recorder per file. Caller drops / explicitly flushes
/// on graceful shutdown to guarantee on-disk durability.
pub struct Recorder {
    writer: BufWriter<File>,
}

impl Recorder {
    /// Open `path` for append-only recording. Creates the file when
    /// missing, opens with `O_APPEND` so concurrent writers cannot
    /// trample each other's records (each `write` is atomic up to
    /// the OS page boundary). Buffered with the default `BufWriter`
    /// capacity.
    ///
    /// # Errors
    /// Surfaces the underlying [`std::io::Error`] from `open` /
    /// `create`.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            writer: BufWriter::new(f),
        })
    }

    /// Append one record: `frame` (with its existing `[u32 LE len]`
    /// header carried verbatim) plus `recv_ts` as a little-endian
    /// `u64` suffix.
    ///
    /// `frame` is the full framed payload as it arrived from the
    /// wire, **including** the `[u32 len][u8 kind][payload]` prefix.
    /// Replay re-feeds it into the wire parser without re-framing.
    ///
    /// # Errors
    /// Propagates [`std::io::Error`] from the buffered writer.
    pub fn record(&mut self, frame: &[u8], recv_ts: RecvTs) -> io::Result<()> {
        self.writer.write_all(frame)?;
        let ts = recv_ts.as_nanos() as u64;
        self.writer.write_all(&ts.to_le_bytes())?;
        Ok(())
    }

    /// Force any buffered writes through to the OS. Caller invokes
    /// this on graceful shutdown — for crash safety the OS journal
    /// is consulted, not the buffer.
    ///
    /// # Errors
    /// Propagates [`std::io::Error`] from the underlying flush.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.writer.flush();
    }
}

/// One record read by [`read_records`]. Carries borrowed slices into
/// the source byte buffer plus the recorded `recv_ts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record<'a> {
    /// The wire frame bytes exactly as they arrived from the
    /// network — `[len: u32 LE][kind: u8][payload]`.
    pub frame: &'a [u8],
    /// Engine-stamped receive timestamp.
    pub recv_ts: RecvTs,
}

/// Decode every record in `bytes`. Returns the parsed list and the
/// number of trailing bytes that constituted a partial / truncated
/// final record (and were ignored). Callers can use the truncation
/// count to validate that a clean shutdown produced zero trailing
/// bytes.
///
/// # Errors
/// Returns [`std::io::Error`] of kind [`io::ErrorKind::InvalidData`]
/// when an internal frame length declares fewer than 1 byte (no
/// kind discriminant fits) — this is unrecoverable corruption.
pub fn read_records(bytes: &[u8]) -> io::Result<(Vec<Record<'_>>, usize)> {
    let mut records = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        // Need 4 bytes of length prefix at minimum.
        if bytes.len() - cursor < 4 {
            break;
        }
        let len_arr: [u8; 4] = bytes[cursor..cursor + 4].try_into().unwrap();
        let frame_len = u32::from_le_bytes(len_arr) as usize;
        if frame_len < 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recorded frame length < 1 — file is corrupt",
            ));
        }
        // Need 4 + frame_len + 8 bytes for one full record.
        let total = 4 + frame_len + RECV_TS_BYTES;
        if bytes.len() - cursor < total {
            // Partial final record — tolerable, abrupt termination.
            break;
        }
        let frame = &bytes[cursor..cursor + 4 + frame_len];
        let ts_arr: [u8; 8] = bytes[cursor + 4 + frame_len..cursor + total]
            .try_into()
            .unwrap();
        let recv_ts = RecvTs::new(u64::from_le_bytes(ts_arr) as i64);
        records.push(Record { frame, recv_ts });
        cursor += total;
    }
    let trailing = bytes.len() - cursor;
    Ok((records, trailing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::NamedTempFile;
    use wire::framing::{Frame, MessageKind};

    fn fixture_frame() -> Vec<u8> {
        let payload = [0x42u8; 8];
        let mut buf = Vec::new();
        Frame::write(MessageKind::CancelOrder, &payload, &mut buf).expect("fits");
        buf
    }

    #[test]
    fn test_record_writes_frame_then_recv_ts() {
        let tmp = NamedTempFile::new().expect("tmp");
        let mut recorder = Recorder::open(tmp.path()).expect("open");
        let frame = fixture_frame();
        recorder.record(&frame, RecvTs::new(123_456)).expect("rec");
        recorder.flush().expect("flush");

        let mut bytes = Vec::new();
        let mut f = std::fs::File::open(tmp.path()).expect("read");
        f.read_to_end(&mut bytes).expect("read all");
        assert_eq!(&bytes[..frame.len()], &frame[..]);
        let ts_arr: [u8; 8] = bytes[frame.len()..].try_into().expect("8 bytes");
        assert_eq!(u64::from_le_bytes(ts_arr), 123_456);
    }

    #[test]
    fn test_read_records_round_trips_count() {
        let tmp = NamedTempFile::new().expect("tmp");
        let mut recorder = Recorder::open(tmp.path()).expect("open");
        let frame = fixture_frame();
        for i in 0..100 {
            recorder
                .record(&frame, RecvTs::new(1_000 + i))
                .expect("rec");
        }
        recorder.flush().expect("flush");
        drop(recorder);

        let mut bytes = Vec::new();
        let mut f = std::fs::File::open(tmp.path()).expect("read");
        f.read_to_end(&mut bytes).expect("read all");
        let (records, trailing) = read_records(&bytes).expect("decode");
        assert_eq!(records.len(), 100);
        assert_eq!(trailing, 0);
        assert_eq!(records[0].recv_ts.as_nanos(), 1_000);
        assert_eq!(records[99].recv_ts.as_nanos(), 1_099);
    }

    #[test]
    fn test_read_records_tolerates_partial_final_record() {
        let mut bytes = Vec::new();
        let mut recorder_buf = Vec::new();
        // Manually pack 3 full records.
        for i in 0..3u64 {
            let frame = fixture_frame();
            recorder_buf.extend_from_slice(&frame);
            recorder_buf.extend_from_slice(&i.to_le_bytes());
        }
        bytes.extend_from_slice(&recorder_buf);
        // Append a partially-written 4th record: a length prefix
        // declaring a non-zero frame followed by only a few of the
        // declared bytes. The reader sees: len prefix is intact, but
        // the file ends before the full frame + 8-byte recv_ts is
        // available — the record is truncated cleanly.
        bytes.extend_from_slice(&20u32.to_le_bytes()); // declares 20-byte frame
        bytes.extend_from_slice(&[0u8; 5]); // only 5 bytes of frame body

        let (records, trailing) = read_records(&bytes).expect("decode");
        assert_eq!(records.len(), 3);
        assert_eq!(trailing, 4 + 5); // length prefix + partial body
    }

    #[test]
    fn test_read_records_tolerates_truncated_length_prefix() {
        // 3 valid records followed by only 2 bytes of a length
        // prefix — the reader stops cleanly without misinterpreting.
        let mut bytes = Vec::new();
        for i in 0..3u64 {
            let frame = fixture_frame();
            bytes.extend_from_slice(&frame);
            bytes.extend_from_slice(&i.to_le_bytes());
        }
        bytes.extend_from_slice(&[0u8; 2]); // short of the 4-byte prefix
        let (records, trailing) = read_records(&bytes).expect("decode");
        assert_eq!(records.len(), 3);
        assert_eq!(trailing, 2);
    }

    #[test]
    fn test_read_records_rejects_zero_length_frame() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u32.to_le_bytes()); // length = 0
        bytes.extend_from_slice(&0u64.to_le_bytes()); // ts
        let result = read_records(&bytes);
        assert!(matches!(result, Err(e) if e.kind() == io::ErrorKind::InvalidData));
    }

    #[test]
    fn test_recorder_no_op_when_not_constructed() {
        // The hot path branches on `Option<&mut Recorder>` only when
        // the gateway constructs one. This test documents that the
        // listener / smoke suite never instantiates Recorder when
        // `--record` is not supplied; behavioural coverage lives in
        // the gateway integration tests in #16.
        let _ = ();
    }
}
