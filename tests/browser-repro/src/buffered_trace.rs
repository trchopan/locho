//! Bounded, disposable trace writer. No per-event file or pipe I/O.
use std::{
    collections::VecDeque,
    io::Write,
    sync::{Arc, Mutex},
};

const MAX_RECORD: usize = 8192;
const MAX_BUFFER: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct State {
    records: VecDeque<Vec<u8>>,
    bytes: usize,
    dropped_bytes: usize,
}

#[derive(Clone, Default)]
pub struct Buffer(Arc<Mutex<State>>);

#[derive(serde::Serialize)]
pub struct Snapshot {
    pub sequence: u64,
    pub dropped_bytes: usize,
    pub data: String,
}

impl Buffer {
    pub fn drain(&self, sequence: u64) -> Snapshot {
        let mut state = self.0.lock().unwrap();
        let data = state.records.drain(..).flatten().collect::<Vec<_>>();
        let dropped_bytes = state.dropped_bytes;
        state.bytes = 0;
        state.dropped_bytes = 0;
        Snapshot {
            sequence,
            dropped_bytes,
            data: String::from_utf8_lossy(&data).into_owned(),
        }
    }
}

pub struct Record {
    buffer: Buffer,
    data: Vec<u8>,
    dropped: usize,
}

impl Write for Record {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = bytes.len().min(MAX_RECORD.saturating_sub(self.data.len()));
        self.data.extend_from_slice(&bytes[..count]);
        self.dropped += bytes.len() - count;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for Record {
    fn drop(&mut self) {
        let mut state = self.buffer.0.lock().unwrap();
        while state.bytes + self.data.len() > MAX_BUFFER {
            let Some(record) = state.records.pop_front() else {
                break;
            };
            state.bytes -= record.len();
            state.dropped_bytes += record.len();
        }
        state.dropped_bytes += self.dropped;
        state.bytes += self.data.len();
        state.records.push_back(std::mem::take(&mut self.data));
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
    type Writer = Record;
    fn make_writer(&'a self) -> Record {
        Record {
            buffer: self.clone(),
            data: Vec::new(),
            dropped: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::fmt::MakeWriter;

    #[test]
    fn records_are_drained_once_and_count_truncation() {
        let buffer = Buffer::default();
        {
            let mut record = buffer.make_writer();
            record.write_all(b"stage=hello\n").unwrap();
        }
        assert_eq!(buffer.drain(1).data, "stage=hello\n");
        assert!(buffer.drain(2).data.is_empty());
        {
            let mut record = buffer.make_writer();
            record.write_all(&vec![b'x'; MAX_RECORD + 7]).unwrap();
        }
        let snapshot = buffer.drain(3);
        assert_eq!(snapshot.sequence, 3);
        assert_eq!(snapshot.data.len(), MAX_RECORD);
        assert_eq!(snapshot.dropped_bytes, 7);
    }

    #[test]
    fn buffer_evicts_old_records_with_an_explicit_drop_count() {
        let buffer = Buffer::default();
        for _ in 0..(MAX_BUFFER / MAX_RECORD + 1) {
            let mut record = buffer.make_writer();
            record.write_all(&vec![b'x'; MAX_RECORD]).unwrap();
        }
        let snapshot = buffer.drain(1);
        assert_eq!(snapshot.data.len(), MAX_BUFFER);
        assert_eq!(snapshot.dropped_bytes, MAX_RECORD);
    }
}
