//! Bounded input and incremental JSON checkpoints. No document is published here.
use crate::load_task::{Phase, Task};
use serde::de::DeserializeOwned;
use std::{fs::File, io::Read, path::Path, sync::atomic::AtomicBool};

const CHUNK: usize = 64 * 1024;
const TOO_LARGE: &str =
    "LOAD_INPUT_TOO_LARGE: Input exceeds limit / 入力サイズが上限を超えました / 输入大小超出限制";
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub task: Option<&'a Task>,
    pub cancel: Option<&'a AtomicBool>,
}
impl Context<'_> {
    pub fn check(self) -> Result<(), String> {
        crate::snapping::check_cancel(self.cancel)?;
        if let Some(task) = self.task {
            task.check()?;
        }
        Ok(())
    }
    pub fn progress(self, phase: Phase, completed: usize, total: usize) -> Result<(), String> {
        self.check()?;
        if let Some(task) = self.task {
            task.progress(phase, completed, total)?;
        }
        Ok(())
    }
}
pub fn read_json<T: DeserializeOwned>(
    path: &Path,
    limit: usize,
    context: Context<'_>,
) -> Result<T, String> {
    context.check()?;
    let file = File::open(path).map_err(|e| e.to_string())?;
    let total = usize::try_from(file.metadata().map_err(|e| e.to_string())?.len())
        .map_err(|_| TOO_LARGE)?;
    let bytes = read_chunks(file, total, limit, context)?;
    parse_json(&bytes, context)
}
fn read_chunks(
    mut reader: impl Read,
    total: usize,
    limit: usize,
    context: Context<'_>,
) -> Result<Vec<u8>, String> {
    context.progress(Phase::Read, 0, total)?;
    if total > limit {
        return Err(TOO_LARGE.into());
    }
    let mut bytes = Vec::new();
    let mut chunk = [0; CHUNK];
    loop {
        context.check()?;
        let take = CHUNK.min(limit.saturating_sub(bytes.len()).saturating_add(1));
        let n = match reader.read(&mut chunk[..take]) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result.map_err(|e| e.to_string())?,
        };
        context.check()?;
        if n == 0 {
            break;
        }
        if bytes.len().saturating_add(n) > limit {
            return Err(TOO_LARGE.into());
        }
        bytes.extend_from_slice(&chunk[..n]);
        context.progress(Phase::Read, bytes.len(), total.max(bytes.len()))?;
    }
    context.progress(Phase::Read, bytes.len(), bytes.len())?;
    Ok(bytes)
}
struct ParseReader<'a> {
    bytes: &'a [u8],
    position: usize,
    checkpoint: usize,
    context: Context<'a>,
}
impl Read for ParseReader<'_> {
    #[inline(always)]
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.checkpoint || self.position == self.bytes.len() {
            self.context
                .progress(Phase::Parse, self.position, self.bytes.len())
                .map_err(std::io::Error::other)?;
            self.checkpoint = self.position.saturating_add(CHUNK);
        }
        // serde_json's IoRead requests one byte at a time. Avoid constructing and
        // copying two dynamic slices on this hot path; keep the same checkpoints.
        if let [byte] = output {
            let Some(value) = self.bytes.get(self.position) else {
                return Ok(0);
            };
            *byte = *value;
            self.position += 1;
            return Ok(1);
        }
        let n = output.len().min(self.bytes.len() - self.position);
        output[..n].copy_from_slice(&self.bytes[self.position..self.position + n]);
        self.position += n;
        Ok(n)
    }
}
pub fn parse_json<T: DeserializeOwned>(bytes: &[u8], context: Context<'_>) -> Result<T, String> {
    context.progress(Phase::Parse, 0, bytes.len())?;
    let result = serde_json::from_reader(ParseReader {
        bytes,
        position: 0,
        checkpoint: CHUNK,
        context,
    });
    // serde adds a byte offset to I/O errors; preserve the shared cancellation contract.
    context.check()?;
    let value = result.map_err(|e| e.to_string())?;
    context.progress(Phase::Parse, bytes.len(), bytes.len())?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load_task::{Kind, Tasks};
    // Keep the previous reader in this benchmark only, so comparisons include
    // identical progress/cancellation work rather than a synthetic no-op reader.
    struct PreviousReader<'a>(ParseReader<'a>);
    impl Read for PreviousReader<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let r = &mut self.0;
            if r.position >= r.checkpoint || r.position == r.bytes.len() {
                r.context
                    .progress(Phase::Parse, r.position, r.bytes.len())
                    .map_err(std::io::Error::other)?;
                r.checkpoint = r.position.saturating_add(CHUNK);
            }
            let n = output.len().min(r.bytes.len() - r.position);
            output[..n].copy_from_slice(&r.bytes[r.position..r.position + n]);
            r.position += n;
            Ok(n)
        }
    }
    #[test]
    #[ignore = "JSON checkpoint overhead comparison; run explicitly"]
    fn json_checkpoint_parse_measurement() {
        let source = vec![[1.25f32, -2.5, 3.75]; 100_000];
        let bytes = serde_json::to_vec(&source).unwrap();
        let tasks = Tasks::default();
        let guard = tasks.begin(Kind::Open).unwrap();
        let context = Context {
            task: Some(&guard.task),
            cancel: Some(guard.task.cancel_flag()),
        };
        let mut samples = Vec::new();
        for run in 0..5 {
            let started = std::time::Instant::now();
            let baseline: Vec<[f32; 3]> = serde_json::from_slice(&bytes).unwrap();
            let slice_us = started.elapsed().as_micros();
            let timed = |optimized| {
                let started = std::time::Instant::now();
                let value: Vec<[f32; 3]> = if optimized {
                    parse_json(&bytes, context).unwrap()
                } else {
                    serde_json::from_reader(PreviousReader(ParseReader {
                        bytes: &bytes,
                        position: 0,
                        checkpoint: CHUNK,
                        context,
                    }))
                    .unwrap()
                };
                let elapsed = started.elapsed().as_micros();
                assert_eq!(value, baseline);
                (optimized, elapsed)
            };
            let first = timed(run % 2 == 0);
            let second = timed(run % 2 != 0);
            let (optimized_us, previous_us) = if first.0 {
                (first.1, second.1)
            } else {
                (second.1, first.1)
            };
            samples.push(serde_json::json!({"slice_us":slice_us,"previous_reader_us":previous_us,"optimized_reader_us":optimized_us}));
        }
        println!(
            "JSON_CHECKPOINT_METRICS {}",
            serde_json::json!({"bytes":bytes.len(),"samples":samples,
            "note":"Synthetic debug five runs with alternating reader order; not a performance guarantee"})
        );
    }
    #[test]
    fn byte_and_bulk_reader_paths_preserve_bytes_and_eof_across_checkpoints() {
        let bytes = "日本語𠮷\"\n".repeat(9000).into_bytes();
        for size in [1, 3, CHUNK, CHUNK + 1] {
            let tasks = Tasks::default();
            let guard = tasks.begin(Kind::Open).unwrap();
            let context = Context {
                task: Some(&guard.task),
                cancel: Some(guard.task.cancel_flag()),
            };
            let mut reader = ParseReader {
                bytes: &bytes,
                position: 0,
                checkpoint: CHUNK,
                context,
            };
            assert_eq!(reader.read(&mut []).unwrap(), 0);
            assert_eq!(reader.position, 0);
            let mut buffer = vec![0; size];
            let mut result = Vec::new();
            loop {
                let n = reader.read(&mut buffer).unwrap();
                if n == 0 {
                    break;
                }
                result.extend_from_slice(&buffer[..n]);
            }
            assert_eq!(result, bytes);
            assert_eq!(reader.read(&mut [0]).unwrap(), 0);
            let snapshot = tasks.snapshot().unwrap();
            assert_eq!(
                (snapshot.completed, snapshot.total),
                (bytes.len(), bytes.len())
            );
        }
    }
    #[test]
    fn bounded_io_and_json_match_slice_parser_and_report_byte_progress() {
        let tasks = Tasks::default();
        let guard = tasks.begin(Kind::Open).unwrap();
        let context = Context {
            task: Some(&guard.task),
            cancel: Some(guard.task.cancel_flag()),
        };
        let value = vec!["日本語中文\n\"".repeat(1000); 20];
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(bytes.len() > CHUNK);
        let read = read_chunks(bytes.as_slice(), bytes.len(), bytes.len(), context).unwrap();
        let progress = tasks.snapshot().unwrap();
        assert_eq!(progress.phase, Phase::Read);
        assert_eq!(
            (progress.completed, progress.total),
            (bytes.len(), bytes.len())
        );
        let decoded: Vec<String> = parse_json(&read, context).unwrap();
        assert_eq!(decoded, value);
        let progress = tasks.snapshot().unwrap();
        assert_eq!(progress.phase, Phase::Parse);
        assert_eq!(
            (progress.completed, progress.total),
            (bytes.len(), bytes.len())
        );
        assert!(read_chunks(bytes.as_slice(), 0, bytes.len() - 1, context).is_err()); // file grew
        assert!(read_chunks(bytes.as_slice(), bytes.len(), 1, context).is_err());
        assert!(parse_json::<serde_json::Value>(b"{} trailing", context).is_err());
        assert!(parse_json::<serde_json::Value>(b"", context).is_err());
    }
    #[test]
    fn cancel_during_io_stops_before_the_next_chunk() {
        let tasks = Tasks::default();
        let guard = tasks.begin(Kind::Recovery).unwrap();
        let id = tasks.snapshot().unwrap().id;
        struct CancelReader<'a> {
            tasks: &'a Tasks,
            id: &'a str,
            reads: usize,
        }
        impl Read for CancelReader<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                self.reads += 1;
                assert_eq!(self.reads, 1);
                bytes.fill(b' ');
                self.tasks.cancel(self.id);
                Ok(bytes.len())
            }
        }
        let mut reader = CancelReader {
            tasks: &tasks,
            id: &id,
            reads: 0,
        };
        let error = read_chunks(
            &mut reader,
            CHUNK * 4,
            CHUNK * 4,
            Context {
                task: Some(&guard.task),
                cancel: None,
            },
        )
        .unwrap_err();
        assert_eq!(error, "LOAD_CANCELLED");
        assert_eq!(reader.reads, 1);
    }
    #[test]
    fn parse_reader_checks_cancellation_inside_a_large_json_token() {
        let tasks = Tasks::default();
        let guard = tasks.begin(Kind::Open).unwrap();
        let context = Context {
            task: Some(&guard.task),
            cancel: None,
        };
        let mut bytes = vec![b'x'; CHUNK * 3];
        bytes[0] = b'"';
        *bytes.last_mut().unwrap() = b'"';
        let mut reader = ParseReader {
            bytes: &bytes,
            position: 0,
            checkpoint: CHUNK,
            context,
        };
        let mut chunk = vec![0; CHUNK];
        assert_eq!(reader.read(&mut chunk).unwrap(), CHUNK);
        tasks.cancel(&tasks.snapshot().unwrap().id);
        assert!(reader
            .read(&mut [0])
            .unwrap_err()
            .to_string()
            .contains("LOAD_CANCELLED"));
        assert_eq!(reader.position, CHUNK);
        let error = parse_json::<String>(&bytes, context).unwrap_err();
        assert_eq!(error, "LOAD_CANCELLED");
    }
}
