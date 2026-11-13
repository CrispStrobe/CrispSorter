use super::*;
use crate::{
    drives::{CloudDrive, DirEntry, DriveType, FileStat},
    sync::transfer_queue::{TransferQueue, TransferState},
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

#[tokio::test]
async fn shared_queue_cancellation_cleans_stage_on_every_backend() {
    let root = tempfile::tempdir().unwrap();
    let target = PullRoot::capture(root.path())
        .unwrap()
        .destination("result")
        .unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered_tx = Mutex::new(Some(entered_tx));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let queue = TransferQueue::with_concurrency(1);
    let job = queue.submit_pull_download(
        "provider".into(),
        "private-source".into(),
        target,
        Some(3),
        move |_, w| {
            w.write_all(b"new")?;
            entered_tx.lock().unwrap().take().unwrap().send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            Ok(3)
        },
    );
    entered_rx.await.unwrap();
    job.cancellation.cancel();
    release_tx.send(()).unwrap();
    assert!(job.handle.await.unwrap().is_err());
    assert_eq!(job.progress_rx.borrow().state, TransferState::Cancelled);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn shared_queue_uncertain_rename_retains_failure_and_later_user_edit() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("result");
    let target = PullRoot::capture(root.path())
        .unwrap()
        .destination("result")
        .unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let hook_path = path.clone();
    let job = TransferQueue::with_concurrency(1).submit_pull_download(
        "provider".into(),
        "private-source".into(),
        target,
        Some(3),
        move |_, w| {
            calls.fetch_add(1, Ordering::SeqCst);
            let path = hook_path.clone();
            PUBLISH_RESULT.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |result| {
                    result.unwrap();
                    std::fs::write(path, b"later user edit").unwrap();
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "lost rename acknowledgement",
                    ))
                }))
            });
            w.write_all(b"new")?;
            Ok(3)
        },
    );
    let error = job.handle.await.unwrap().unwrap_err();
    assert!(error
        .downcast_ref::<crate::drives::NonReplayableWrite>()
        .is_some());
    assert!(matches!(
        job.progress_rx.borrow().state,
        TransferState::Failed { .. }
    ));
    job.cancellation.cancel();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(path).unwrap(), b"later user edit");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn shared_stage_collision_read_error_and_completed_name_reuse_preserve_unowned_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = PullRoot::capture(temp.path()).unwrap();
    let target = root.destination("nested/result").unwrap();
    let parent = temp.path().join("nested");
    std::fs::write(parent.join("collision"), b"existing").unwrap();
    assert!(target
        .stage_named("collision".into(), Some(3), |_| panic!(
            "collision read source"
        ))
        .is_err());
    assert_eq!(
        std::fs::read(parent.join("collision")).unwrap(),
        b"existing"
    );
    assert!(target
        .stage(Some(3), |w| {
            w.write_all(b"n")?;
            anyhow::bail!("source failed")
        })
        .is_err());
    assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 1);
    let stage = target
        .stage_named("former".into(), Some(3), |w| {
            w.write_all(b"new")?;
            Ok(3)
        })
        .unwrap();
    let former = parent.join("former");
    PUBLISH_RESULT.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |result| {
            result.as_ref().unwrap();
            std::fs::write(former, b"unrelated").unwrap();
            result
        }))
    });
    stage.publish().unwrap();
    assert_eq!(std::fs::read(parent.join("former")).unwrap(), b"unrelated");
    assert_eq!(std::fs::read(parent.join("result")).unwrap(), b"new");
    for relative in ["../escape", "a//b", "nested/../escape", "", "a\\b"] {
        assert!(root.destination(relative).is_err());
    }
}

struct PairDrive {
    local: std::path::PathBuf,
    moved: std::path::PathBuf,
    move_on_list: bool,
    fail_second: bool,
}
impl PairDrive {
    #[cfg(unix)]
    fn replace_root(&self) {
        std::fs::rename(&self.local, &self.moved).unwrap();
        std::fs::create_dir(&self.local).unwrap();
        for name in ["one", "two"] {
            std::fs::write(self.local.join(name), b"replacement").unwrap();
        }
    }
}
impl CloudDrive for PairDrive {
    fn label(&self) -> &str {
        "pair-drive"
    }
    fn drive_type(&self) -> DriveType {
        DriveType::Local
    }
    fn list_dir(&self, _: &Path) -> Result<Vec<DirEntry>> {
        #[cfg(unix)]
        if self.move_on_list {
            self.replace_root();
        }
        Ok(["one", "two"]
            .into_iter()
            .map(|name| DirEntry {
                name: name.into(),
                is_dir: false,
                size: Some(3),
            })
            .collect())
    }
    fn stat(&self, _: &Path) -> Result<FileStat> {
        Ok(FileStat {
            size: 3,
            is_dir: false,
            mtime_unix: Some(10),
        })
    }
    fn read_file_to_writer(&self, path: &Path, writer: &mut dyn Write) -> Result<u64> {
        let second = path.file_name().unwrap() == "two";
        if self.fail_second && second {
            anyhow::bail!("provider rejection");
        }
        writer.write_all(if second { b"two" } else { b"one" })?;
        #[cfg(unix)]
        if !self.move_on_list && !second && !self.fail_second {
            self.replace_root();
        }
        Ok(3)
    }
    fn read_file(&self, _: &Path) -> Result<Vec<u8>> {
        panic!("pair used whole-file API")
    }
    fn write_file(&self, _: &Path, _: &[u8]) -> Result<()> {
        panic!("pair uploaded bytes")
    }
    fn delete(&self, _: &Path) -> Result<()> {
        panic!("pair deleted bytes")
    }
}

#[tokio::test]
async fn production_pull_loop_error_does_not_report_batch_success() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("local");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("two"), b"previous").unwrap();
    let root = PullRoot::capture(&path).unwrap();
    let drive = Arc::new(PairDrive {
        local: path.clone(),
        moved: temp.path().join("moved"),
        move_on_list: false,
        fail_second: true,
    });
    let entries = ["one", "two"].map(|name| crate::sync::pairs::SyncRemoteEntry {
        relative_path: name.into(),
        size: 3,
        mtime_unix: Some(10),
    });
    let result = download_entries(
        &TransferQueue::with_concurrency(1),
        "provider",
        drive,
        "/remote",
        &entries,
        root,
    )
    .await;
    assert!(result.is_err());
    assert_eq!(std::fs::read(path.join("one")).unwrap(), b"one");
    assert_eq!(std::fs::read(path.join("two")).unwrap(), b"previous");
    assert_eq!(std::fs::read_dir(path).unwrap().count(), 2);
}

#[cfg(unix)]
#[tokio::test]
async fn production_pull_loop_reuses_root_after_inventory_or_first_stream_replacement() {
    for move_on_list in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("local");
        std::fs::create_dir(&path).unwrap();
        let moved = temp.path().join("moved");
        let root = PullRoot::capture(&path).unwrap();
        let drive = Arc::new(PairDrive {
            local: path.clone(),
            moved: moved.clone(),
            move_on_list,
            fail_second: false,
        });
        let mut entries = Vec::new();
        crate::sync::inventory::inventory_remote(
            drive.as_ref(),
            Path::new("/remote"),
            "",
            &[],
            &[],
            &mut entries,
        )
        .unwrap();
        assert_eq!(
            download_entries(
                &TransferQueue::with_concurrency(1),
                "provider",
                drive,
                "/remote",
                &entries,
                root
            )
            .await
            .unwrap(),
            2
        );
        assert_eq!(std::fs::read(moved.join("one")).unwrap(), b"one");
        assert_eq!(std::fs::read(moved.join("two")).unwrap(), b"two");
        for name in ["one", "two"] {
            assert_eq!(std::fs::read(path.join(name)).unwrap(), b"replacement");
        }
        assert_eq!(std::fs::read_dir(moved).unwrap().count(), 2);
    }
}
