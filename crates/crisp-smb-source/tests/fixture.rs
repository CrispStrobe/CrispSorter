use crisp_smb_source::{Config, SmbSource, CHUNK};
use std::io::{Cursor, Write};

struct VerifyingWriter {
    count: usize,
    max_chunk: usize,
}
impl Write for VerifyingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        assert!(bytes.len() <= CHUNK);
        self.max_chunk = self.max_chunk.max(bytes.len());
        for (i, byte) in bytes.iter().enumerate() {
            assert_eq!(*byte, ((self.count + i) % 251) as u8);
        }
        self.count += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
#[ignore = "Run scripts/test_smb_source.py with the pinned CrispCloud checkout"]
async fn direct_core_streams_without_flutter_or_whole_file_buffers() {
    let address = std::env::var("CRISP_SMB_TEST_ADDRESS").unwrap();
    let source = SmbSource::new(Config {
        address: address.clone(),
        share: "books".into(),
        root: "".into(),
        username: "reader".into(),
        password: "fixture-password".into(),
        domain: "".into(),
        require_encryption: false,
    })
    .unwrap();
    let mut writer = VerifyingWriter {
        count: 0,
        max_chunk: 0,
    };
    let count = source
        .read_to_writer("fixture.bin", &mut writer)
        .await
        .unwrap();
    assert_eq!(count, 3 * 1024 * 1024 + 17);
    assert_eq!(writer.max_chunk, CHUNK);
    assert_eq!(source.list("").await.unwrap().as_array().unwrap().len(), 1);
    assert!(source.stat("missing.bin").await.is_err());

    struct FailedWriter(usize);
    impl Write for FailedWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            self.0 += 1;
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "consumer stopped"))
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut failed = FailedWriter(0);
    assert!(source.read_to_writer("fixture.bin", &mut failed).await.is_err());
    assert_eq!(failed.0, 1, "a failed consumer must stop the producer immediately");
    // The failed reader must release its remote handle and leave the session
    // usable. Empty files still exercise open/close, validation and publication.
    source.write_from_reader("empty.bin", &mut Cursor::new(Vec::<u8>::new()), 0).await.unwrap();
    assert_eq!(source.read_to_writer("empty.bin", &mut Vec::new()).await.unwrap(), 0);
    assert!(source.write_from_reader("short.bin", &mut Cursor::new(vec![1]), 2).await.is_err());
    assert!(source.write_from_reader("long.bin", &mut Cursor::new(vec![1, 2]), 1).await.is_err());
    let mut bytes = Cursor::new(vec![1, 2, 3, 4]);
    source
        .write_from_reader("uploaded.bin", &mut bytes, 4)
        .await
        .unwrap();
    assert_eq!(source.stat("uploaded.bin").await.unwrap()["size"], 4);
    let strict = SmbSource::new(Config {
        address,
        share: "books".into(),
        root: "".into(),
        username: "reader".into(),
        password: "fixture-password".into(),
        domain: "".into(),
        require_encryption: true,
    })
    .unwrap();
    assert!(
        strict.list("").await.is_err(),
        "SMB2 fixture cannot satisfy required encryption"
    );
    strict.close().await;
    source.cancel();
    assert_eq!(
        source.stat("fixture.bin").await.unwrap_err().code,
        "Cancelled"
    );
    source.close().await;
}
