use crispcat::{
    caf,
    index::{FileEntry, FileIndex},
    scan::{scan_dir, ScanOptions},
};
use std::path::PathBuf;
#[test]
fn scan_and_caf_preserve_empty_directories_and_times() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(root.join("empty/nested")).unwrap();
    std::fs::write(root.join("zero"), []).unwrap();
    let index = scan_dir(&root, ScanOptions::default()).unwrap();
    assert_eq!(index.directories.len(), 2);
    let output = temp.path().join("index.caf");
    caf::write_file(&output, &index, 1700000000).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    let loaded = caf::read_file(&output).unwrap();
    let mut before = index.directories;
    let mut after = loaded.directories;
    before.sort();
    after.sort();
    assert_eq!(before, after);
    assert_eq!(loaded.all_files[0].size, 0);
}
#[test]
fn cancelled_read_or_write_does_not_replace_catalogue() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("index.caf");
    let mut index = FileIndex::new(PathBuf::from("/offline"), false);
    index.add(FileEntry::new(
        PathBuf::from("/offline/empty.txt"),
        0,
        1700000000,
    ));
    index
        .directories
        .push((PathBuf::from("/offline/empty"), 1700000009));
    caf::write_file(&output, &index, 1700000000).unwrap();
    let old = std::fs::read(&output).unwrap();
    assert!(caf::read_file_cancellable(&output, &|| true).is_err());
    assert!(caf::write_file_cancellable(&output, &index, 1700000000, &|| true).is_err());
    assert_eq!(std::fs::read(&output).unwrap(), old);
}
