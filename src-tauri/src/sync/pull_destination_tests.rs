use super::*;
use crate::sync::transfer_queue::{TransferCancellation, TransferQueue, TransferState};
use std::{
    os::unix::fs::symlink,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

#[derive(Clone)]
struct Swap {
    source: std::path::PathBuf,
    moved: std::path::PathBuf,
    replacement: std::path::PathBuf,
}
impl Swap {
    fn perform(&self) {
        std::fs::rename(&self.source, &self.moved).unwrap();
        symlink(&self.replacement, &self.source).unwrap();
    }
}

#[tokio::test]
async fn actual_queue_root_parent_swaps_before_read_during_stream_and_before_publish() {
    for move_root in [false, true] {
        for when in ["queued", "stream", "publish"] {
            let temp = tempfile::tempdir().unwrap();
            let configured = temp.path().join("configured");
            std::fs::create_dir_all(configured.join("nested")).unwrap();
            let replacement = temp.path().join("replacement");
            std::fs::create_dir_all(replacement.join("nested")).unwrap();
            let protected = replacement.join(if move_root { "nested/result" } else { "result" });
            std::fs::write(&protected, b"protected").unwrap();
            let moved = temp.path().join("moved");
            let swap = Swap {
                source: if move_root {
                    configured.clone()
                } else {
                    configured.join("nested")
                },
                moved: moved.clone(),
                replacement,
            };
            let root = PullRoot::capture(&configured).unwrap();
            let target = root.destination("nested/result").unwrap();
            let queue = TransferQueue::with_concurrency(1);
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let entered_tx = Mutex::new(Some(entered_tx));
            let release_rx = Mutex::new(release_rx);
            let blocker =
                queue.submit_download("blocker".into(), "blocker".into(), None, move |_| {
                    entered_tx.lock().unwrap().take().unwrap().send(()).unwrap();
                    release_rx.lock().unwrap().recv().unwrap();
                    Ok(vec![])
                });
            entered_rx.await.unwrap();
            let stream_swap = swap.clone();
            let job = queue.submit_pull_download(
                "provider".into(),
                "source".into(),
                target,
                Some(3),
                move |_, writer| {
                    writer.write_all(b"n")?;
                    if when == "stream" {
                        stream_swap.perform();
                    }
                    if when == "publish" {
                        let swap = stream_swap.clone();
                        BEFORE_PUBLISH.with(|slot| {
                            *slot.borrow_mut() = Some(Box::new(move || swap.perform()))
                        });
                    }
                    writer.write_all(b"ew")?;
                    Ok(3)
                },
            );
            if when == "queued" {
                swap.perform();
            }
            release_tx.send(()).unwrap();
            blocker.handle.await.unwrap().unwrap();
            assert_eq!(job.handle.await.unwrap().unwrap(), 3);
            let parent = if move_root {
                moved.join("nested")
            } else {
                moved.clone()
            };
            assert_eq!(std::fs::read(parent.join("result")).unwrap(), b"new");
            assert_eq!(std::fs::read(protected).unwrap(), b"protected");
            assert_eq!(
                std::fs::read_dir(parent).unwrap().count(),
                1,
                "stage leaked after relocation"
            );
            if move_root {
                // A later entry derives from the same root, not its replaced path.
                let next = root.destination("nested/second").unwrap();
                next.stage(Some(3), |w| {
                    w.write_all(b"two")?;
                    Ok(3)
                })
                .unwrap()
                .publish()
                .unwrap();
                assert_eq!(std::fs::read(moved.join("nested/second")).unwrap(), b"two");
                assert!(!configured.join("nested/second").exists());
            }
        }
    }
}

#[tokio::test]
async fn retry_reuses_captured_objects_and_cleans_partial_stages_after_root_move() {
    let temp = tempfile::tempdir().unwrap();
    let root_path = temp.path().join("root");
    std::fs::create_dir(&root_path).unwrap();
    let replacement = temp.path().join("replacement");
    std::fs::create_dir(&replacement).unwrap();
    std::fs::write(replacement.join("result"), b"protected").unwrap();
    let moved = temp.path().join("moved");
    let swap = Swap {
        source: root_path.clone(),
        moved: moved.clone(),
        replacement: replacement.clone(),
    };
    let root = PullRoot::capture(&root_path).unwrap();
    let target = root.destination("result").unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let job = TransferQueue::with_concurrency(1).submit_pull_download(
        "provider".into(),
        "source".into(),
        target,
        Some(3),
        move |_, w| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                w.write_all(b"n")?;
                swap.perform();
                anyhow::bail!("temporary connection failure");
            }
            w.write_all(b"new")?;
            Ok(3)
        },
    );
    assert_eq!(job.handle.await.unwrap().unwrap(), 3);
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(std::fs::read(moved.join("result")).unwrap(), b"new");
    assert_eq!(
        std::fs::read(replacement.join("result")).unwrap(),
        b"protected"
    );
    assert_eq!(std::fs::read_dir(moved).unwrap().count(), 1);
}

#[tokio::test]
async fn cancellation_after_stream_and_parent_relocation_cleans_original_object() {
    let temp = tempfile::tempdir().unwrap();
    let root_path = temp.path().join("root");
    std::fs::create_dir(&root_path).unwrap();
    let moved = temp.path().join("moved");
    let replacement = temp.path().join("replacement");
    std::fs::create_dir(&replacement).unwrap();
    std::fs::write(replacement.join("result"), b"protected").unwrap();
    let target = PullRoot::capture(&root_path)
        .unwrap()
        .destination("result")
        .unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Mutex::new(Some(started_tx));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let job = TransferQueue::with_concurrency(1).submit_pull_download(
        "provider".into(),
        "source".into(),
        target,
        Some(3),
        move |_, w| {
            w.write_all(b"new")?;
            started_tx.lock().unwrap().take().unwrap().send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            Ok(3)
        },
    );
    started_rx.await.unwrap();
    Swap {
        source: root_path,
        moved: moved.clone(),
        replacement: replacement.clone(),
    }
    .perform();
    job.cancellation.cancel();
    release_tx.send(()).unwrap();
    assert!(job.handle.await.unwrap().is_err());
    assert_eq!(job.progress_rx.borrow().state, TransferState::Cancelled);
    assert_eq!(std::fs::read_dir(moved).unwrap().count(), 0);
    assert_eq!(
        std::fs::read(replacement.join("result")).unwrap(),
        b"protected"
    );
}

#[tokio::test]
async fn uncertain_real_rename_is_not_retried_or_relabelled_cancelled() {
    for race_cancel in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("result");
        let target = PullRoot::capture(temp.path())
            .unwrap()
            .destination("result")
            .unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<TransferCancellation>();
        let cancel_rx = Mutex::new(cancel_rx);
        let threads = Arc::new(Mutex::new(Vec::new()));
        let spawned = threads.clone();
        let hook_path = destination.clone();
        let job = TransferQueue::with_concurrency(1).submit_pull_download(
            "provider".into(),
            "source".into(),
            target,
            Some(3),
            move |_, w| {
                calls.fetch_add(1, Ordering::SeqCst);
                let cancellation = cancel_rx.lock().unwrap().recv().unwrap();
                let spawned = spawned.clone();
                let path = hook_path.clone();
                PUBLISH_RESULT.with(|slot| {
                    *slot.borrow_mut() = Some(Box::new(move |result| {
                        result.unwrap();
                        assert_eq!(std::fs::read(&path).unwrap(), b"new");
                        std::fs::write(path, b"later edit").unwrap();
                        if race_cancel {
                            let (tx, rx) = std::sync::mpsc::channel();
                            spawned.lock().unwrap().push(std::thread::spawn(move || {
                                tx.send(()).unwrap();
                                cancellation.cancel();
                            }));
                            rx.recv().unwrap();
                        }
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
        cancel_tx.send(job.cancellation.clone()).unwrap();
        drop(cancel_tx);
        let error = job.handle.await.unwrap().unwrap_err();
        for thread in threads.lock().unwrap().drain(..) {
            thread.join().unwrap();
        }
        assert!(error
            .downcast_ref::<crate::drives::NonReplayableWrite>()
            .is_some());
        assert!(matches!(
            job.progress_rx.borrow().state,
            TransferState::Failed { .. }
        ));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(destination).unwrap(), b"later edit");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}

#[test]
fn aliases_nested_creation_leaf_links_permissions_and_size_validation() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let alias = temp.path().join("alias");
    symlink(&real, &alias).unwrap();
    let root = PullRoot::capture(&alias).unwrap();
    let target = root.destination("nested/result").unwrap();
    let outside = temp.path().join("outside");
    std::fs::write(&outside, b"protected").unwrap();
    symlink(&outside, real.join("link")).unwrap();
    assert!(root.destination("link").is_err());
    symlink(temp.path(), real.join("escape")).unwrap();
    assert!(root.destination("escape/child").is_err());
    for invalid in ["../escape", "nested/../escape", "a//b", "", "a\\b"] {
        assert!(root.destination(invalid).is_err());
    }
    assert!(target
        .stage(Some(2), |w| {
            w.write_all(b"new")?;
            Ok(3)
        })
        .is_err());
    assert!(target
        .stage(Some(4), |w| {
            w.write_all(b"new")?;
            Ok(3)
        })
        .is_err());
    assert_eq!(std::fs::read_dir(real.join("nested")).unwrap().count(), 0);
    let stage = target
        .stage(Some(3), |w| {
            w.write_all(b"new")?;
            Ok(3)
        })
        .unwrap();
    let leaf = real.join("nested/result");
    let referent = outside.clone();
    BEFORE_PUBLISH
        .with(|hook| *hook.borrow_mut() = Some(Box::new(move || symlink(referent, leaf).unwrap())));
    stage.publish().unwrap();
    assert_eq!(std::fs::read(&outside).unwrap(), b"protected");
    assert!(!std::fs::symlink_metadata(real.join("nested/result"))
        .unwrap()
        .file_type()
        .is_symlink());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(real.join("nested/result"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn stage_collision_and_reused_former_name_never_remove_unowned_entries() {
    let temp = tempfile::tempdir().unwrap();
    let target = PullRoot::capture(temp.path())
        .unwrap()
        .destination("result")
        .unwrap();
    std::fs::write(temp.path().join("collision"), b"existing user file").unwrap();
    assert!(target
        .stage_named("collision".into(), Some(3), |_| panic!(
            "collision read source"
        ))
        .is_err());
    assert_eq!(
        std::fs::read(temp.path().join("collision")).unwrap(),
        b"existing user file"
    );
    let stage = target
        .stage_named("former-stage".into(), Some(3), |w| {
            w.write_all(b"new")?;
            Ok(3)
        })
        .unwrap();
    let former = temp.path().join("former-stage");
    PUBLISH_RESULT.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |result| {
            result.as_ref().unwrap();
            std::fs::write(former, b"new unrelated entry").unwrap();
            result
        }))
    });
    stage.publish().unwrap();
    assert_eq!(
        std::fs::read(temp.path().join("former-stage")).unwrap(),
        b"new unrelated entry"
    );
    assert_eq!(std::fs::read(temp.path().join("result")).unwrap(), b"new");
}
