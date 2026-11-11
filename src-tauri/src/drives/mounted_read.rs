//! Bounded reads for OS-visible files, including SMB/NFS mounts.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub(super) fn read_to_writer(path: &Path, writer: &mut dyn Write) -> io::Result<u64> {
    let mut file = std::fs::File::open(path)?;
    io::copy(&mut file, writer)
}

/// An inclusive, bounded slice for mounted-file FUSE reads. Seeking avoids
/// reading or caching all bytes before the requested page.
pub(super) fn read_range(path: &Path, start: u64, end: u64) -> io::Result<Vec<u8>> {
    let length = end.checked_sub(start).and_then(|value| value.checked_add(1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid mounted byte range"))?;
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(length).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "crispsorter-mounted-read-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn file(&self) -> PathBuf {
            self.0.join("source.bin")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mounted_range_returns_requested_slice_and_clamps_at_eof() {
        let fixture = Fixture::new();
        fs::write(fixture.file(), b"0123456789").unwrap();
        assert_eq!(read_range(&fixture.file(), 3, 5).unwrap(), b"345");
        assert_eq!(read_range(&fixture.file(), 8, 50).unwrap(), b"89");
        assert!(read_range(&fixture.file(), 20, 30).unwrap().is_empty());
        assert_eq!(read_range(&fixture.file(), 5, 2).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn streams_large_file_in_bounded_chunks() {
        let fixture = Fixture::new();
        let block = vec![37; 64 * 1024];
        let mut source = File::create(fixture.file()).unwrap();
        for _ in 0..128 {
            source.write_all(&block).unwrap();
        }
        drop(source);

        struct Counter {
            bytes: usize,
            largest: usize,
        }
        impl Write for Counter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                assert!(buf.iter().all(|&b| b == 37));
                self.bytes += buf.len();
                self.largest = self.largest.max(buf.len());
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut output = Counter {
            bytes: 0,
            largest: 0,
        };
        assert_eq!(
            read_to_writer(&fixture.file(), &mut output).unwrap(),
            8 * 1024 * 1024
        );
        assert_eq!(output.bytes, 8 * 1024 * 1024);
        assert!(
            output.largest <= 64 * 1024,
            "whole-file buffering regressed"
        );
    }

    #[test]
    fn empty_and_missing_sources() {
        let fixture = Fixture::new();
        let mut output = Vec::new();
        assert_eq!(
            read_to_writer(&fixture.file(), &mut output)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        File::create(fixture.file()).unwrap();
        assert_eq!(read_to_writer(&fixture.file(), &mut output).unwrap(), 0);
        assert!(output.is_empty());
    }

    #[test]
    fn propagates_writer_failure_without_reading_the_rest() {
        let fixture = Fixture::new();
        fs::write(fixture.file(), vec![42; 1024 * 1024]).unwrap();
        struct FailingWriter(usize);
        impl Write for FailingWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                self.0 += 1;
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "consumer stopped",
                ))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut output = FailingWriter(0);
        assert_eq!(
            read_to_writer(&fixture.file(), &mut output)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(output.0, 1);
    }

    #[test]
    fn handles_short_destination_writes() {
        let fixture = Fixture::new();
        fs::write(fixture.file(), b"mounted share").unwrap();
        struct ShortWriter(Vec<u8>);
        impl Write for ShortWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let len = buf.len().min(2);
                self.0.extend_from_slice(&buf[..len]);
                Ok(len)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut output = ShortWriter(Vec::new());
        assert_eq!(read_to_writer(&fixture.file(), &mut output).unwrap(), 13);
        assert_eq!(output.0, b"mounted share");
    }
}
