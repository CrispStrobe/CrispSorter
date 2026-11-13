//! Limits apply to bytes read from reqwest's decoded response, before JSON
//! deserialization. Neither Content-Length nor peer block counts allocate memory.
use std::io::{self, Read};

pub(super) const STATUS_LIMIT: usize = 64 * 1024;
// A byte budget, not a file-size limit: compact maps with 100,000 signatures
// (over 390 GiB with the normal 4 MiB blocks) fit comfortably. Larger/verbose
// maps require a full transfer. Keep this finite even for legitimate huge files.
pub(super) const BLOCK_MAP_LIMIT: usize = 32 * 1024 * 1024;

pub(super) fn read_bounded(mut reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut scratch = [0u8; 8192];
    loop {
        // Probe one byte beyond the budget, including when exactly at the
        // limit. Do not drain an oversized or endless response.
        let remaining = limit - bytes.len();
        let request = scratch.len().min(remaining.saturating_add(1));
        let count = match reader.read(&mut scratch[..request]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Ok(bytes);
        }
        if count > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("delta metadata exceeds the {limit}-byte decoded response limit; use a full transfer"),
            ));
        }
        bytes.extend_from_slice(&scratch[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn exact_limit_empty_and_short_reads_are_accepted() {
        for size in [0, 1, 8191, 8192, 8193] {
            let data = vec![b'a'; size];
            assert_eq!(read_bounded(Cursor::new(&data), size).unwrap(), data);
            assert_eq!(read_bounded(Cursor::new(&data), size + 1).unwrap(), data);
        }
        struct Chunks(Cursor<Vec<u8>>);
        impl Read for Chunks {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let length = buf.len().min(3);
                self.0.read(&mut buf[..length])
            }
        }
        assert_eq!(
            read_bounded(Chunks(Cursor::new(b"valid input".to_vec())), 11).unwrap(),
            b"valid input"
        );
    }

    #[test]
    fn oversized_and_endless_streams_stop_at_one_byte_over_budget() {
        for limit in [0, 1, 8191, 8192, 8193, STATUS_LIMIT] {
            let mut source = Cursor::new(vec![b'a'; limit + 100]);
            let error = read_bounded(&mut source, limit).unwrap_err();
            assert_eq!(source.position(), (limit + 1) as u64);
            assert!(error.to_string().contains(&format!("{limit}-byte")));
            assert!(error.to_string().contains("use a full transfer"));
        }
        assert!(read_bounded(io::repeat(b'a'), 12).is_err());
    }

    #[test]
    fn partial_read_failure_is_preserved_and_interrupted_reads_retry() {
        struct Fails(u8);
        impl Read for Fails {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.0 += 1;
                match self.0 {
                    1 => Err(io::ErrorKind::Interrupted.into()),
                    2 => {
                        buf[0] = b'a';
                        Ok(1)
                    }
                    _ => Err(io::Error::other("broken response")),
                }
            }
        }
        let error = read_bounded(Fails(0), 100).unwrap_err();
        assert_eq!(error.to_string(), "broken response");
    }
}
