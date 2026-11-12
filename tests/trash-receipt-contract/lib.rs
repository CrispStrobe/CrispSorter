//! Real journal and native receipt logic with an in-memory SDK transport.
#[path = "../../src-tauri/src/drives/trash_receipts.rs"]
pub mod trash_receipts;

#[cfg(test)]
mod adapter {
    use super::trash_receipts::ReceiptStore;
    use anyhow::{anyhow, Result};
    use serde::{Deserialize, Serialize};
    use std::{collections::HashMap, path::Path, sync::Mutex};

    #[derive(Clone)]
    struct InternxtSession {
        drive_api_url: String,
        user_id: String,
        root_folder_id: String,
    }
    #[derive(Clone)]
    struct NativeItem {
        name: String,
        uuid: String,
        is_dir: bool,
    }
    #[derive(Default)]
    struct Remote {
        folders: HashMap<String, Vec<NativeItem>>,
        trash: Vec<NativeItem>,
        deletes: usize,
        restores: usize,
        fail_delete_after: bool,
        fail_restore_after: bool,
        fail_delete_before: bool,
        fail_restore_before: bool,
        pause_delete_before: Option<(
            std::sync::Arc<std::sync::Barrier>,
            std::sync::Arc<std::sync::Barrier>,
        )>,
        pause_delete_after: Option<(
            std::sync::Arc<std::sync::Barrier>,
            std::sync::Arc<std::sync::Barrier>,
        )>,
    }
    struct InternxtNativeClient(Mutex<Remote>);
    impl InternxtNativeClient {
        fn list_folder(&self, id: &str) -> Result<Vec<NativeItem>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .folders
                .get(id)
                .cloned()
                .unwrap_or_default())
        }
        fn list_trash(&self, _: Option<&str>, _: usize) -> Result<Vec<NativeItem>> {
            Ok(self.0.lock().unwrap().trash.clone())
        }
        fn trash_once(&self, id: &str, kind: &str) -> Result<()> {
            let pause = self.0.lock().unwrap().pause_delete_before.clone();
            if let Some((entered, release)) = pause {
                entered.wait();
                release.wait();
            }
            let mut remote = self.0.lock().unwrap();
            anyhow::ensure!(
                !remote.fail_delete_before,
                "deletion refused before mutation"
            );
            let mut found = None;
            for items in remote.folders.values_mut() {
                if let Some(index) = items.iter().position(|item| item.uuid == id) {
                    found = Some(items.remove(index));
                    break;
                }
            }
            let item = found.ok_or_else(|| anyhow!("missing object"))?;
            assert_eq!(kind, if item.is_dir { "folder" } else { "file" });
            remote.trash.push(item);
            remote.deletes += 1;
            let fail_after = remote.fail_delete_after;
            let pause = remote.pause_delete_after.clone();
            drop(remote);
            if let Some((entered, release)) = pause {
                entered.wait();
                release.wait();
            }
            anyhow::ensure!(!fail_after, "response lost after deletion");
            Ok(())
        }
        fn restore_from_trash_once(&self, id: &str, kind: &str, parent: &str) -> Result<()> {
            let mut remote = self.0.lock().unwrap();
            anyhow::ensure!(
                !remote.fail_restore_before,
                "restoration refused before mutation"
            );
            let index = remote
                .trash
                .iter()
                .position(|item| item.uuid == id)
                .ok_or_else(|| anyhow!("missing object"))?;
            let item = remote.trash.remove(index);
            assert_eq!(kind, if item.is_dir { "folders" } else { "files" });
            remote.folders.entry(parent.into()).or_default().push(item);
            remote.restores += 1;
            anyhow::ensure!(
                !remote.fail_restore_after,
                "response lost after restoration"
            );
            Ok(())
        }
    }
    struct NativeInternxtDrive {
        session: Mutex<InternxtSession>,
        client: InternxtNativeClient,
    }
    impl NativeInternxtDrive {
        fn with_client<T>(
            &self,
            _: Option<()>,
            op: impl FnOnce(&InternxtSession, &InternxtNativeClient) -> Result<T>,
        ) -> Result<T> {
            let session = self.session.lock().unwrap().clone();
            op(&session, &self.client)
        }
    }
    include!(concat!(env!("OUT_DIR"), "/internxt.rs"));

    fn item(id: &str, name: &str, is_dir: bool) -> NativeItem {
        NativeItem {
            uuid: id.into(),
            name: name.into(),
            is_dir,
        }
    }
    fn drive() -> NativeInternxtDrive {
        NativeInternxtDrive {
            session: Mutex::new(InternxtSession {
                drive_api_url: "https://provider.example".into(),
                user_id: "account-a".into(),
                root_folder_id: "root-a".into(),
            }),
            client: InternxtNativeClient(Mutex::new(Remote {
                folders: HashMap::from([
                    ("root-a".into(), vec![item("folder-a", "Docs", true)]),
                    (
                        "folder-a".into(),
                        vec![item("object-a", "report.pdf", false)],
                    ),
                ]),
                ..Default::default()
            })),
        }
    }

    #[cfg(unix)]
    #[test]
    fn exact_object_survives_restart_and_same_name_trash_never_substitutes() {
        let dir = tempfile::tempdir().unwrap();
        let provider = drive();
        provider
            .client
            .0
            .lock()
            .unwrap()
            .trash
            .push(item("other-object", "report.pdf", false));
        let receipt = ReceiptStore::open(dir.path())
            .unwrap()
            .trash(
                &provider,
                "drive-a",
                "group-a",
                Path::new("/Docs/report.pdf"),
            )
            .unwrap();
        assert_eq!(receipt.state, "deleted");
        let store = ReceiptStore::open(dir.path()).unwrap();
        assert_eq!(store.list("group-a").unwrap().len(), 1);
        assert!(store.list("other-group").unwrap().is_empty());
        assert_eq!(
            store
                .restore(&provider, "drive-a", &receipt.id)
                .unwrap()
                .state,
            "restored"
        );
        assert!(store.restore(&provider, "drive-a", &receipt.id).is_err());
        let remote = provider.client.0.lock().unwrap();
        assert_eq!(remote.restores, 1);
        assert_eq!(remote.deletes, 1);
        assert_eq!(remote.trash[0].uuid, "other-object");
        assert_eq!(remote.folders["folder-a"][0].uuid, "object-a");
    }

    #[cfg(unix)]
    #[test]
    fn cross_drive_account_endpoint_and_root_mismatches_never_mutate() {
        let dir = tempfile::tempdir().unwrap();
        let provider = drive();
        let store = ReceiptStore::open(dir.path()).unwrap();
        let receipt = store
            .trash(
                &provider,
                "drive-a",
                "group-a",
                Path::new("/Docs/report.pdf"),
            )
            .unwrap();
        assert!(store.restore(&provider, "drive-b", &receipt.id).is_err());
        for field in ["user", "endpoint", "root"] {
            {
                let mut session = provider.session.lock().unwrap();
                match field {
                    "user" => session.user_id = "account-b".into(),
                    "endpoint" => session.drive_api_url = "https://other.example".into(),
                    _ => session.root_folder_id = "root-b".into(),
                };
            }
            assert!(store.restore(&provider, "drive-a", &receipt.id).is_err());
            let mut session = provider.session.lock().unwrap();
            session.user_id = "account-a".into();
            session.drive_api_url = "https://provider.example".into();
            session.root_folder_id = "root-a".into();
        }
        assert_eq!(provider.client.0.lock().unwrap().restores, 0);
        assert_eq!(
            store
                .restore(&provider, "drive-a", &receipt.id)
                .unwrap()
                .state,
            "restored"
        );
    }

    #[cfg(unix)]
    #[test]
    fn lost_delete_response_recovers_by_observation_without_another_delete() {
        let dir = tempfile::tempdir().unwrap();
        let provider = drive();
        provider.client.0.lock().unwrap().fail_delete_after = true;
        let store = ReceiptStore::open(dir.path()).unwrap();
        assert!(store
            .trash(
                &provider,
                "drive-a",
                "group-a",
                Path::new("/Docs/report.pdf")
            )
            .is_err());
        let receipt = store.list("group-a").unwrap().pop().unwrap();
        assert_eq!(receipt.state, "delete_uncertain");
        assert!(store.restore(&provider, "drive-a", &receipt.id).is_err());
        let reopened = ReceiptStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened
                .reconcile(&provider, "drive-a", &receipt.id)
                .unwrap()
                .state,
            "deleted"
        );
        assert_eq!(provider.client.0.lock().unwrap().deletes, 1);
        reopened.restore(&provider, "drive-a", &receipt.id).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn lost_restore_response_is_reconciled_without_replaying_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let provider = drive();
        let store = ReceiptStore::open(dir.path()).unwrap();
        let receipt = store
            .trash(
                &provider,
                "drive-a",
                "group-a",
                Path::new("/Docs/report.pdf"),
            )
            .unwrap();
        provider.client.0.lock().unwrap().fail_restore_after = true;
        assert!(store.restore(&provider, "drive-a", &receipt.id).is_err());
        assert!(store.restore(&provider, "drive-a", &receipt.id).is_err());
        assert_eq!(
            store
                .reconcile(&provider, "drive-a", &receipt.id)
                .unwrap()
                .state,
            "restored"
        );
        assert_eq!(provider.client.0.lock().unwrap().restores, 1);
    }

    #[cfg(unix)]
    #[test]
    fn changed_uuid_kind_name_parent_and_destination_collision_fail_closed() {
        for change in ["uuid", "kind", "name", "parent", "collision"] {
            let dir = tempfile::tempdir().unwrap();
            let provider = drive();
            let store = ReceiptStore::open(dir.path()).unwrap();
            let receipt = store
                .trash(
                    &provider,
                    "drive-a",
                    "group-a",
                    Path::new("/Docs/report.pdf"),
                )
                .unwrap();
            {
                let mut remote = provider.client.0.lock().unwrap();
                match change {
                    "uuid" => remote.trash[0].uuid = "replacement".into(),
                    "kind" => remote.trash[0].is_dir = true,
                    "name" => remote.trash[0].name = "renamed.pdf".into(),
                    "parent" => {
                        remote.folders.get_mut("root-a").unwrap()[0].uuid =
                            "replacement-folder".into()
                    }
                    _ => remote.folders.get_mut("folder-a").unwrap().push(item(
                        "replacement",
                        "report.pdf",
                        false,
                    )),
                }
            }
            assert!(
                store.restore(&provider, "drive-a", &receipt.id).is_err(),
                "{change}"
            );
            assert_eq!(provider.client.0.lock().unwrap().restores, 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn ambiguous_source_and_failed_journal_never_delete() {
        let dir = tempfile::tempdir().unwrap();
        let provider = drive();
        let store = ReceiptStore::open(dir.path()).unwrap();
        provider
            .client
            .0
            .lock()
            .unwrap()
            .folders
            .get_mut("folder-a")
            .unwrap()
            .push(item("other", "report.pdf", false));
        assert!(store
            .trash(
                &provider,
                "drive-a",
                "group-a",
                Path::new("/Docs/report.pdf")
            )
            .is_err());
        assert_eq!(provider.client.0.lock().unwrap().deletes, 0);
        provider
            .client
            .0
            .lock()
            .unwrap()
            .folders
            .get_mut("folder-a")
            .unwrap()
            .pop();
        std::fs::remove_dir(dir.path().join("trash-receipts")).unwrap();
        assert!(store
            .trash(
                &provider,
                "drive-a",
                "group-a",
                Path::new("/Docs/report.pdf")
            )
            .is_err());
        assert_eq!(provider.client.0.lock().unwrap().deletes, 0);
    }

    #[cfg(unix)]
    #[test]
    fn legacy_paths_and_arbitrary_receipt_ids_cannot_restore() {
        let dir = tempfile::tempdir().unwrap();
        let provider = drive();
        let store = ReceiptStore::open(dir.path()).unwrap();
        for id in [
            "/Docs/report.pdf",
            "../../secret",
            "missing",
            "00000000-0000-0000-0000-000000000000",
        ] {
            assert!(store.restore(&provider, "drive-a", id).is_err());
        }
        assert_eq!(provider.client.0.lock().unwrap().restores, 0);
    }
    #[cfg(unix)]
    #[test]
    fn pre_mutation_crashes_reconcile_without_replaying_delete_or_restore() {
        let dir = tempfile::tempdir().unwrap();
        let provider = drive();
        let store = ReceiptStore::open(dir.path()).unwrap();
        provider.client.0.lock().unwrap().fail_delete_before = true;
        assert!(store
            .trash(
                &provider,
                "drive-a",
                "group-a",
                Path::new("/Docs/report.pdf")
            )
            .is_err());
        let abandoned = store.list("group-a").unwrap().pop().unwrap();
        assert_eq!(
            store
                .reconcile(&provider, "drive-a", &abandoned.id)
                .unwrap()
                .state,
            "delete_uncertain"
        );
        assert_eq!(provider.client.0.lock().unwrap().deletes, 0);
        provider.client.0.lock().unwrap().fail_delete_before = false;
        let receipt = store
            .trash(
                &provider,
                "drive-a",
                "group-a",
                Path::new("/Docs/report.pdf"),
            )
            .unwrap();
        provider.client.0.lock().unwrap().fail_restore_before = true;
        assert!(store.restore(&provider, "drive-a", &receipt.id).is_err());
        assert_eq!(
            store
                .reconcile(&provider, "drive-a", &receipt.id)
                .unwrap()
                .state,
            "restore_uncertain"
        );
        assert!(store.restore(&provider, "drive-a", &receipt.id).is_err());
        assert_eq!(provider.client.0.lock().unwrap().restores, 0);
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_restore_uses_a_single_durable_claim() {
        let dir = tempfile::tempdir().unwrap();
        let provider = std::sync::Arc::new(drive());
        let receipt = ReceiptStore::open(dir.path())
            .unwrap()
            .trash(
                provider.as_ref(),
                "drive-a",
                "group-a",
                Path::new("/Docs/report.pdf"),
            )
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let attempts: Vec<_> = (0..2)
            .map(|_| {
                let provider = provider.clone();
                let root = dir.path().to_owned();
                let id = receipt.id.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let store = ReceiptStore::open(&root).unwrap();
                    barrier.wait();
                    store.restore(provider.as_ref(), "drive-a", &id).is_ok()
                })
            })
            .collect();
        let completed = attempts
            .into_iter()
            .map(|attempt| usize::from(attempt.join().unwrap()))
            .sum::<usize>();
        assert_eq!(completed, 1);
        assert_eq!(provider.client.0.lock().unwrap().restores, 1);
    }

    #[cfg(unix)]
    #[test]
    fn directory_receipts_preserve_object_kind_and_private_file_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let provider = drive();
        let store = ReceiptStore::open(dir.path()).unwrap();
        provider
            .client
            .0
            .lock()
            .unwrap()
            .folders
            .get_mut("folder-a")
            .unwrap()[0]
            .is_dir = true;
        let receipt = store
            .trash(
                &provider,
                "drive-a",
                "group-a",
                Path::new("/Docs/report.pdf"),
            )
            .unwrap();
        store.restore(&provider, "drive-a", &receipt.id).unwrap();
        assert!(provider.client.0.lock().unwrap().folders["folder-a"][0].is_dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for entry in std::fs::read_dir(dir.path().join("trash-receipts")).unwrap() {
                assert_eq!(
                    entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
    }
    #[cfg(unix)]
    #[test]
    fn incomplete_provider_trash_metadata_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let provider = drive();
        let store = ReceiptStore::open(dir.path()).unwrap();
        let receipt = store
            .trash(
                &provider,
                "drive-a",
                "group-a",
                Path::new("/Docs/report.pdf"),
            )
            .unwrap();
        // A malformed or regressed provider response must never weaken identity checks.
        provider.client.0.lock().unwrap().trash[0].name = "report".into();
        assert!(store.restore(&provider, "drive-a", &receipt.id).is_err());
        assert_eq!(provider.client.0.lock().unwrap().restores, 0);
    }
    #[test]
    fn app_exposes_receipt_restore_separately_from_legacy_path_restore() {
        let provider = drive();
        assert_eq!(provider.capabilities().trash_receipts, cfg!(unix));
        assert!(!provider.capabilities().reversible_trash);
        assert_eq!(provider.receipt_trash().is_some(), cfg!(unix));
    }
    #[cfg(unix)]
    #[test]
    fn reconcile_cannot_terminally_classify_an_in_flight_delete() {
        let dir = tempfile::tempdir().unwrap();
        let provider = std::sync::Arc::new(drive());
        let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::Barrier::new(2));
        provider.client.0.lock().unwrap().pause_delete_before =
            Some((entered.clone(), release.clone()));
        let task = {
            let root = dir.path().to_owned();
            let provider = provider.clone();
            std::thread::spawn(move || {
                ReceiptStore::open(&root).unwrap().trash(
                    provider.as_ref(),
                    "drive-a",
                    "group-a",
                    Path::new("/Docs/report.pdf"),
                )
            })
        };
        entered.wait();
        let store = ReceiptStore::open(dir.path()).unwrap();
        let receipt = store.list("group-a").unwrap().pop().unwrap();
        assert_eq!(
            store
                .reconcile(provider.as_ref(), "drive-a", &receipt.id)
                .unwrap()
                .state,
            "delete_uncertain"
        );
        assert!(!dir
            .path()
            .join("trash-receipts")
            .join(format!("{}.not-deleted", receipt.id))
            .exists());
        release.wait();
        assert_eq!(task.join().unwrap().unwrap().state, "deleted");
        assert_eq!(
            store
                .restore(provider.as_ref(), "drive-a", &receipt.id)
                .unwrap()
                .state,
            "restored"
        );
        assert_eq!(provider.client.0.lock().unwrap().deletes, 1);
    }

    #[cfg(unix)]
    #[test]
    fn reconcile_success_before_delete_response_is_idempotently_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let provider = std::sync::Arc::new(drive());
        let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::Barrier::new(2));
        provider.client.0.lock().unwrap().pause_delete_after =
            Some((entered.clone(), release.clone()));
        let task = {
            let root = dir.path().to_owned();
            let provider = provider.clone();
            std::thread::spawn(move || {
                ReceiptStore::open(&root).unwrap().trash(
                    provider.as_ref(),
                    "drive-a",
                    "group-a",
                    Path::new("/Docs/report.pdf"),
                )
            })
        };
        entered.wait();
        let store = ReceiptStore::open(dir.path()).unwrap();
        let receipt = store.list("group-a").unwrap().pop().unwrap();
        assert_eq!(
            store
                .reconcile(provider.as_ref(), "drive-a", &receipt.id)
                .unwrap()
                .state,
            "deleted"
        );
        // Old journals could contain this advisory marker; it must not mask a confirmed deletion.
        std::fs::write(
            dir.path()
                .join("trash-receipts")
                .join(format!("{}.not-deleted", receipt.id)),
            b"",
        )
        .unwrap();
        release.wait();
        assert_eq!(task.join().unwrap().unwrap().state, "deleted");
        assert_eq!(
            store
                .restore(provider.as_ref(), "drive-a", &receipt.id)
                .unwrap()
                .state,
            "restored"
        );
    }

    #[cfg(unix)]
    #[test]
    fn oversized_serialized_receipt_is_rejected_before_remote_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let provider = drive();
        let store = ReceiptStore::open(dir.path()).unwrap();
        let error = store
            .trash(
                &provider,
                "drive-a",
                &"g".repeat(65536),
                Path::new("/Docs/report.pdf"),
            )
            .unwrap_err();
        assert!(error.to_string().contains("too large"));
        assert_eq!(provider.client.0.lock().unwrap().deletes, 0);
        assert_eq!(
            std::fs::read_dir(dir.path().join("trash-receipts"))
                .unwrap()
                .count(),
            0
        );
    }
}
