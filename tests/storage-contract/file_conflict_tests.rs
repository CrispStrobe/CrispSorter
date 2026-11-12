//! Production-source tests: generated modules contain the real pair planner and
//! file mutation decisions, not reimplementations or substituted providers.
use crate::sync::{conflict::ConflictPolicy, pairs::*};

fn local(path: &str, size: u64, time: i64) -> SyncPlanEntry {
    SyncPlanEntry {
        relative_path: path.into(),
        size,
        mtime_unix: time,
    }
}
fn remote(path: &str, size: u64, time: Option<i64>) -> SyncRemoteEntry {
    SyncRemoteEntry {
        relative_path: path.into(),
        size,
        mtime_unix: time,
    }
}

#[test]
fn newest_requires_evidence_of_timestamp_order() {
    for size in [1, 2] {
        for time in [None, Some(-1), Some(0), Some(10), Some(20)] {
            let result = compare_plans(
                &[local("report", 1, 10)],
                &[remote("report", size, time)],
                ConflictPolicy::NewestWins,
            );
            let expected = match time {
                Some(0) => SyncComparisonAction::ManualReview,
                Some(10) if size == 1 => SyncComparisonAction::Unchanged,
                Some(value) if value < 10 => SyncComparisonAction::UseLocal,
                Some(value) if value > 10 => SyncComparisonAction::UseRemote,
                _ => SyncComparisonAction::ManualReview,
            };
            assert_eq!(
                result[0].action, expected,
                "remote size={size}, time={time:?}"
            );
        }
    }
}

#[test]
fn ambiguous_newest_batch_rejects_before_any_upload_or_watermark_advance() {
    let local = [
        local("missing-time-private-name", 1, 10),
        local("same-time", 1, 10),
        local("later-new-file", 1, 99),
    ];
    let remote = [
        remote("missing-time-private-name", 2, None),
        remote("same-time", 2, Some(10)),
    ];
    // Returning an error instead of a partial upload plan prevents the caller
    // from advancing to 99 and silently stranding the conflicts behind it.
    let error = local_push_plan(&local, &remote, ConflictPolicy::NewestWins)
        .unwrap_err()
        .to_string();
    assert!(error.contains("2 file conflict(s)"));
    assert!(!error.contains("private-name"));
}

#[test]
fn known_newer_and_new_files_remain_eligible_while_remote_newer_is_preserved() {
    let local = [
        local("local-newer", 1, 20),
        local("remote-newer", 1, 10),
        local("same", 1, 10),
        local("new", 1, 99),
    ];
    let remote = [
        remote("local-newer", 2, Some(10)),
        remote("remote-newer", 2, Some(20)),
        remote("same", 1, Some(10)),
    ];
    let plan = local_push_plan(&local, &remote, ConflictPolicy::NewestWins).unwrap();
    assert_eq!(
        plan.iter()
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["local-newer", "new"]
    );
}

#[test]
fn explicit_resolution_policies_are_preserved_for_ambiguous_metadata() {
    for time in [None, Some(10)] {
        for (policy, expected) in [
            (ConflictPolicy::LocalWins, SyncComparisonAction::UseLocal),
            (ConflictPolicy::RemoteWins, SyncComparisonAction::UseRemote),
            (ConflictPolicy::KeepBoth, SyncComparisonAction::KeepBoth),
            (ConflictPolicy::Manual, SyncComparisonAction::ManualReview),
        ] {
            let local = [local("report", 1, 10)];
            let remote = [remote("report", 2, time)];
            assert_eq!(compare_plans(&local, &remote, policy)[0].action, expected);
            assert_eq!(
                local_push_plan(&local, &remote, policy).unwrap().len(),
                usize::from(policy == ConflictPolicy::LocalWins)
            );
        }
    }
}

#[test]
fn zero_time_sentinels_cannot_order_or_short_circuit_newest_conflicts() {
    for local_time in [0, 10] {
        for remote_time in [None, Some(-1), Some(0), Some(10)] {
            if local_time != 0 && remote_time != Some(0) {
                continue;
            }
            for size in [1, 2] {
                let local = [local("epoch", 1, local_time)];
                let remote = [remote("epoch", size, remote_time)];
                assert_eq!(
                    compare_plans(&local, &remote, ConflictPolicy::NewestWins)[0].action,
                    SyncComparisonAction::ManualReview
                );
                assert!(local_push_plan(&local, &remote, ConflictPolicy::NewestWins).is_err());
            }
        }
    }
    // Other policies keep their previous unchanged-metadata fast path.
    for policy in [
        ConflictPolicy::LocalWins,
        ConflictPolicy::RemoteWins,
        ConflictPolicy::KeepBoth,
        ConflictPolicy::Manual,
    ] {
        assert_eq!(
            compare_plans(
                &[local("epoch", 1, 0)],
                &[remote("epoch", 1, Some(0))],
                policy
            )[0]
            .action,
            SyncComparisonAction::Unchanged
        );
    }
    // Zero is the sentinel; a negative timestamp can still carry an ordering.
    assert_eq!(
        compare_plans(
            &[local("pre-epoch", 1, -1)],
            &[remote("pre-epoch", 2, Some(-2))],
            ConflictPolicy::NewestWins
        )[0]
        .action,
        SyncComparisonAction::UseLocal
    );
}

#[test]
fn real_scanner_keeps_unknown_time_files_after_the_incremental_watermark() {
    use std::time::{Duration, UNIX_EPOCH};
    let root = tempfile::tempdir().unwrap();
    for (name, seconds) in [("epoch", 0), ("old", 5), ("boundary", 10), ("new", 20)] {
        let file = std::fs::File::create(root.path().join(name)).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
            .unwrap();
    }
    let pair = SyncPair {
        id: "pair".into(),
        local_root: root.path().to_string_lossy().into_owned(),
        drive_id: "drive".into(),
        remote_root: "/".into(),
        mode: SyncPairMode::ToCloud,
        include_globs: vec![],
        exclude_globs: vec![],
        watermark: 10,
        enabled: true,
        updated_at: 0,
    };
    let plan = plan_local_since(&pair).unwrap();
    assert_eq!(
        plan.iter()
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["boundary", "epoch", "new"]
    );
    assert_eq!(
        plan.iter()
            .find(|entry| entry.relative_path == "epoch")
            .unwrap()
            .mtime_unix,
        0
    );
    assert!(local_push_plan(
        &plan,
        &[remote("epoch", 0, Some(0))],
        ConflictPolicy::NewestWins
    )
    .is_err());
}
