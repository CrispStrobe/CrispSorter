//! Provider-independent scheduling policy for backup jobs.
//!
//! This module deliberately does not spawn a background task or perform I/O.
//! It gives CLI, Tauri, and a future launch-agent worker the same due/next-wake
//! calculation while execution remains an explicit caller decision.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::backup_state::{BackupJob, BackupState};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupSchedulerSnapshot {
    pub now: i64,
    pub due_job_ids: Vec<String>,
    pub next_wake_at: Option<i64>,
}

pub struct BackupScheduler;

impl BackupScheduler {
    pub fn snapshot(state: &BackupState, now: i64) -> Result<BackupSchedulerSnapshot> {
        let jobs = state.list_jobs()?;
        let due_job_ids = jobs.iter()
            .filter(|job| job.enabled && job.schedule.next_due_at(now, job.last_run_at).is_some_and(|at| at <= now))
            .map(|job| job.id.clone())
            .collect();
        let next_wake_at = jobs.iter()
            .filter(|job| job.enabled)
            .filter_map(|job| job.schedule.next_due_at(now, job.last_run_at))
            .filter(|at| *at > now)
            .min();
        Ok(BackupSchedulerSnapshot { now, due_job_ids, next_wake_at })
    }

    pub fn due_jobs(state: &BackupState, now: i64) -> Result<Vec<BackupJob>> {
        Ok(Self::snapshot(state, now)?.due_job_ids.into_iter()
            .filter_map(|id| state.job(&id).ok().flatten())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::backup_state::{BackupSchedule, BackupJob};

    #[test]
    fn snapshot_reports_due_and_future_jobs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = BackupState::open(tmp.path()).unwrap();
        state.upsert_job(BackupJob {
            id: "due".into(), source_root: "/src".into(), drive_id: "d".into(),
            remote_root: "/dst".into(), schedule: BackupSchedule::IntervalMinutes { minutes: 1 },
            retention_count: 1, verify_integrity: true, enabled: true,
            last_run_at: None, last_status: None, updated_at: 0,
        }).unwrap();
        state.upsert_job(BackupJob {
            id: "manual".into(), source_root: "/src".into(), drive_id: "d".into(),
            remote_root: "/dst".into(), schedule: BackupSchedule::Manual,
            retention_count: 1, verify_integrity: true, enabled: true,
            last_run_at: None, last_status: None, updated_at: 0,
        }).unwrap();
        let snapshot = BackupScheduler::snapshot(&state, 1_700_000_000_000).unwrap();
        assert_eq!(snapshot.due_job_ids, vec!["due"]);
        assert!(snapshot.next_wake_at.is_none());
    }
}
