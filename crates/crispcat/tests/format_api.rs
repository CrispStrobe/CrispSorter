use crispcat::scan::{scan_dir, ScanOptions};
use crispcat_format::{caf, index::FileIndex};

#[test]
fn standalone_codec_types_and_existing_scanner_api_interoperate() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(root.join("empty")).unwrap();
    std::fs::write(root.join("zero.txt"), []).unwrap();
    std::fs::write(root.join("note.txt"), b"catalogue payload").unwrap();

    // Public type identity is preserved without serializing or converting it.
    let scanned: FileIndex = scan_dir(&root, ScanOptions::default()).unwrap();
    let output = temp.path().join("portable.caf");
    caf::write_file(&output, &scanned, 1700000000).unwrap();
    let existing: crispcat::index::FileIndex = crispcat::caf::read_file(&output).unwrap();
    assert_eq!(existing.all_files.len(), 2);
    assert_eq!(existing.directories.len(), 1);
    assert!(existing.all_files.iter().any(|file| file.size == 0));

    // Both API paths produce precisely the same interchange bytes.
    let other = temp.path().join("consumer.caf");
    crispcat::caf::write_file(&other, &existing, 1700000000).unwrap();
    assert_eq!(std::fs::read(output).unwrap(), std::fs::read(other).unwrap());
}
