use crate::operation_log::OperationLog;
use serde_json::{json, Value};

fn summarize(total: usize, scores: impl Iterator<Item = Option<f64>>) -> Value {
    let mut scores: Vec<_> = scores.flatten().filter(|score| (0.0..=1.0).contains(score)).collect();
    scores.sort_unstable_by(f64::total_cmp);
    let mean = (!scores.is_empty()).then(|| scores.iter().sum::<f64>() / scores.len() as f64);
    let p10 = scores.get(scores.len().saturating_sub(1) / 10).copied();
    json!({"count":total,"scored_units":scores.len(),"confidence_mean":mean,"confidence_p10":p10})
}

pub(super) fn record(
    log: &OperationLog,
    engine: Option<&str>,
    unit: &str,
    total: usize,
    scores: impl Iterator<Item = Option<f64>>,
) {
    if !crate::operation_log::enabled("debug") {
        return;
    }
    let mut summary = summarize(total, scores);
    summary["engine"] = json!(engine);
    summary["score_unit"] = json!(unit);
    log.step("recognition_quality", summary);
}

pub(super) fn record_crispembed(log: &OperationLog, snapshot: Option<&Value>, requested_engine: Option<&str>, total: usize) {
    let engine = snapshot.and_then(|s| s.get("engine")).and_then(Value::as_str).or(requested_engine);
    let regions = snapshot.and_then(|s| s.get("regions")).and_then(Value::as_array);
    record(log, engine, "region", total, regions.into_iter().flatten().map(|region| {
        (region.get("confidence_source").and_then(Value::as_str) == Some("recognizer"))
            .then(|| region.get("recognition_confidence").and_then(Value::as_f64)).flatten()
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_summary_keeps_zero_and_reports_missing_scores() {
        assert_eq!(summarize(5, [None, Some(-1.0), Some(f64::NAN), Some(0.0), Some(1.0)].into_iter()),
            json!({"count":5,"scored_units":2,"confidence_mean":0.5,"confidence_p10":0.0}));
        assert_eq!(summarize(2, [None, None].into_iter()),
            json!({"count":2,"scored_units":0,"confidence_mean":null,"confidence_p10":null}));
    }
}
