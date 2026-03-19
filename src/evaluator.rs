use std::collections::HashMap;
use chrono::{DateTime, Utc};
use crate::hash::hash_string;
use crate::types::*;

fn get_bucket_value(
    flag_key: &str,
    context: &EvaluationContext,
    rollout: &SnapshotRollout,
    inputs_used: &mut Vec<String>,
) -> i32 {
    let input_key = format!("{}.{}", rollout.bucket_context_kind, rollout.bucket_attribute_key);
    if !inputs_used.contains(&input_key) {
        inputs_used.push(input_key);
    }

    let bucket_key = context
        .get(&rollout.bucket_context_kind)
        .and_then(|ctx| ctx.get(&rollout.bucket_attribute_key))
        .and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Bool(b) => Some(b.to_string()),
            serde_json::Value::Null => None,
            _ => Some(v.to_string()),
        })
        .unwrap_or_else(|| "anonymous".to_string());

    let seed = rollout.seed.as_deref().unwrap_or("");
    let hash_input = format!("{}:{}:{}", flag_key, seed, bucket_key);
    (hash_string(&hash_input) % 100_000) as i32
}

struct ActiveRollout {
    variations: Vec<SnapshotRolloutVariation>,
    step_index: i32,
}

fn resolve_active_rollout(rollout: &SnapshotRollout, now: DateTime<Utc>) -> ActiveRollout {
    let schedule = match &rollout.schedule {
        Some(s) if !s.is_empty() => s,
        _ => return ActiveRollout { variations: rollout.variations.clone(), step_index: -1 },
    };

    let started_at_str = match &rollout.started_at {
        Some(s) if !s.is_empty() => s,
        _ => return ActiveRollout { variations: rollout.variations.clone(), step_index: -1 },
    };

    let started_at = match started_at_str.parse::<DateTime<Utc>>() {
        Ok(t) => t,
        Err(_) => return ActiveRollout { variations: rollout.variations.clone(), step_index: -1 },
    };

    let elapsed_ms = (now - started_at).num_milliseconds() as f64;
    let elapsed_minutes = elapsed_ms / 60_000.0;

    if elapsed_minutes < 0.0 {
        return ActiveRollout {
            variations: schedule[0].variations.clone(),
            step_index: 0,
        };
    }

    let mut cumulative_minutes = 0.0;
    for (i, step) in schedule.iter().enumerate() {
        if step.duration_minutes == 0 {
            return ActiveRollout {
                variations: step.variations.clone(),
                step_index: i as i32,
            };
        }
        cumulative_minutes += step.duration_minutes as f64;
        if elapsed_minutes < cumulative_minutes {
            return ActiveRollout {
                variations: step.variations.clone(),
                step_index: i as i32,
            };
        }
    }

    let last = &schedule[schedule.len() - 1];
    ActiveRollout {
        variations: last.variations.clone(),
        step_index: (schedule.len() - 1) as i32,
    }
}

struct RolloutResult {
    variation: SnapshotVariation,
    variation_key: String,
    matched_weight: i32,
    bucket_value: i32,
    step_index: i32,
}

fn select_variation_from_rollout(
    active_variations: &[SnapshotRolloutVariation],
    bucket_value: i32,
    variations: &HashMap<String, SnapshotVariation>,
    step_index: i32,
) -> Option<RolloutResult> {
    let mut cumulative = 0;
    let mut matched: Option<&SnapshotRolloutVariation> = None;

    for rv in active_variations {
        cumulative += rv.weight;
        if bucket_value < cumulative {
            matched = Some(rv);
            break;
        }
    }

    if matched.is_none() && !active_variations.is_empty() {
        matched = active_variations.last();
    }

    let matched = matched?;
    let variation = variations.get(&matched.variation_key)?;

    Some(RolloutResult {
        variation: variation.clone(),
        variation_key: matched.variation_key.clone(),
        matched_weight: matched.weight,
        bucket_value,
        step_index,
    })
}

fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

fn values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    // Numeric comparison: compare as f64 if both are numbers
    if let (Some(an), Some(bn)) = (a.as_f64(), b.as_f64()) {
        return an == bn;
    }
    value_to_string(a) == value_to_string(b)
}

fn contains_op(context_value: &serde_json::Value, condition_value: &serde_json::Value) -> bool {
    if let (Some(cs), Some(vs)) = (context_value.as_str(), condition_value.as_str()) {
        return cs.contains(vs);
    }
    if let Some(arr) = context_value.as_array() {
        for item in arr {
            if values_equal(item, condition_value) {
                return true;
            }
        }
    }
    false
}

fn in_op(context_value: &serde_json::Value, condition_value: &serde_json::Value) -> bool {
    if let Some(arr) = condition_value.as_array() {
        for item in arr {
            if values_equal(item, context_value) {
                return true;
            }
        }
    }
    false
}

fn compare_numbers(a: &serde_json::Value, b: &serde_json::Value, cmp: fn(f64, f64) -> bool) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(af), Some(bf)) => cmp(af, bf),
        _ => false,
    }
}

fn evaluate_condition(
    condition: &SnapshotRuleCondition,
    context: &EvaluationContext,
    inputs_used: &mut Vec<String>,
) -> bool {
    let input_key = format!("{}.{}", condition.context_kind, condition.attribute_key);
    if !inputs_used.contains(&input_key) {
        inputs_used.push(input_key);
    }

    let context_value = context
        .get(&condition.context_kind)
        .and_then(|ctx| ctx.get(&condition.attribute_key));

    // Handle exists/not_exists early — they don't need a non-null value
    match condition.operator {
        TargetingOperator::Exists => {
            return context_value.is_some() && !context_value.unwrap().is_null();
        }
        TargetingOperator::NotExists => {
            return context_value.is_none() || context_value.unwrap().is_null();
        }
        _ => {}
    }

    // For all other operators, we need a non-null context value
    let cv = match context_value {
        Some(v) if !v.is_null() => v,
        _ => return false,
    };

    match condition.operator {
        TargetingOperator::Equals => values_equal(cv, &condition.value),
        TargetingOperator::NotEquals => !values_equal(cv, &condition.value),
        TargetingOperator::Contains => contains_op(cv, &condition.value),
        TargetingOperator::NotContains => !contains_op(cv, &condition.value),
        TargetingOperator::StartsWith => {
            match (cv.as_str(), condition.value.as_str()) {
                (Some(cs), Some(vs)) => cs.starts_with(vs),
                _ => false,
            }
        }
        TargetingOperator::EndsWith => {
            match (cv.as_str(), condition.value.as_str()) {
                (Some(cs), Some(vs)) => cs.ends_with(vs),
                _ => false,
            }
        }
        TargetingOperator::GreaterThan => compare_numbers(cv, &condition.value, |a, b| a > b),
        TargetingOperator::LessThan => compare_numbers(cv, &condition.value, |a, b| a < b),
        TargetingOperator::GreaterThanOrEqual => compare_numbers(cv, &condition.value, |a, b| a >= b),
        TargetingOperator::LessThanOrEqual => compare_numbers(cv, &condition.value, |a, b| a <= b),
        TargetingOperator::In => in_op(cv, &condition.value),
        TargetingOperator::NotIn => !in_op(cv, &condition.value),
        TargetingOperator::Exists => !cv.is_null(),
        TargetingOperator::NotExists => cv.is_null(),
    }
}

fn evaluate_conditions(
    conditions: &[SnapshotRuleCondition],
    context: &EvaluationContext,
    inputs_used: &mut Vec<String>,
) -> bool {
    conditions.iter().all(|c| evaluate_condition(c, context, inputs_used))
}

fn matches_individual(
    entries: &[SnapshotIndividualEntry],
    context: &EvaluationContext,
    inputs_used: &mut Vec<String>,
) -> bool {
    for entry in entries {
        let input_key = format!("{}.{}", entry.context_kind, entry.attribute_key);
        if !inputs_used.contains(&input_key) {
            inputs_used.push(input_key);
        }
        if let Some(ctx) = context.get(&entry.context_kind) {
            if let Some(val) = ctx.get(&entry.attribute_key) {
                if value_to_string(val) == entry.attribute_value {
                    return true;
                }
            }
        }
    }
    false
}

fn evaluate_segment(
    segment: &SnapshotSegment,
    context: &EvaluationContext,
    inputs_used: &mut Vec<String>,
) -> bool {
    if !segment.excluded.is_empty() && matches_individual(&segment.excluded, context, inputs_used) {
        return false;
    }
    if !segment.included.is_empty() && matches_individual(&segment.included, context, inputs_used) {
        return true;
    }
    if segment.conditions.is_empty() {
        return false;
    }
    evaluate_conditions(&segment.conditions, context, inputs_used)
}

fn evaluate_target(
    target: &SnapshotTarget,
    context: &EvaluationContext,
    segments: &HashMap<String, SnapshotSegment>,
    inputs_used: &mut Vec<String>,
) -> bool {
    match target.target_type.as_str() {
        "individual" => {
            let ck = match &target.context_kind {
                Some(s) if !s.is_empty() => s,
                _ => return false,
            };
            let ak = match &target.attribute_key {
                Some(s) if !s.is_empty() => s,
                _ => return false,
            };
            let av = match &target.attribute_value {
                Some(s) => s,
                _ => return false,
            };
            let input_key = format!("{}.{}", ck, ak);
            if !inputs_used.contains(&input_key) {
                inputs_used.push(input_key);
            }
            if let Some(ctx) = context.get(ck.as_str()) {
                if let Some(val) = ctx.get(ak.as_str()) {
                    return value_to_string(val) == *av;
                }
            }
            false
        }
        "rule" => {
            if let Some(conditions) = &target.conditions {
                if !conditions.is_empty() {
                    return evaluate_conditions(conditions, context, inputs_used);
                }
            }
            false
        }
        "segment" => {
            if let Some(segment_key) = &target.segment_key {
                if let Some(segment) = segments.get(segment_key) {
                    return evaluate_segment(segment, context, inputs_used);
                }
            }
            false
        }
        _ => false,
    }
}

fn resolve_target_variation(
    target: &SnapshotTarget,
    flag_key: &str,
    context: &EvaluationContext,
    variations: &HashMap<String, SnapshotVariation>,
    inputs_used: &mut Vec<String>,
    now: DateTime<Utc>,
) -> Option<RolloutResult> {
    if let Some(rollout) = &target.rollout {
        let active = resolve_active_rollout(rollout, now);
        let bucket_value = get_bucket_value(flag_key, context, rollout, inputs_used);
        return select_variation_from_rollout(&active.variations, bucket_value, variations, active.step_index);
    }

    if let Some(vk) = &target.variation_key {
        if let Some(variation) = variations.get(vk) {
            return Some(RolloutResult {
                variation: variation.clone(),
                variation_key: vk.clone(),
                matched_weight: 100_000,
                bucket_value: 0,
                step_index: -1,
            });
        }
    }

    None
}

fn resolve_default_variation(
    flag: &SnapshotFlag,
    context: &EvaluationContext,
    inputs_used: &mut Vec<String>,
    now: DateTime<Utc>,
) -> Option<RolloutResult> {
    if let Some(rollout) = &flag.default_rollout {
        let active = resolve_active_rollout(rollout, now);
        let bucket_value = get_bucket_value(&flag.key, context, rollout, inputs_used);
        return select_variation_from_rollout(&active.variations, bucket_value, &flag.variations, active.step_index);
    }

    if let Some(dvk) = &flag.default_variation_key {
        if let Some(variation) = flag.variations.get(dvk) {
            return Some(RolloutResult {
                variation: variation.clone(),
                variation_key: dvk.clone(),
                matched_weight: 100_000,
                bucket_value: 0,
                step_index: -1,
            });
        }
    }

    None
}

fn build_rule_match_reason(target: &SnapshotTarget) -> Reason {
    Reason {
        reason_type: "rule_match".to_string(),
        rule_id: target.id.clone(),
        rule_name: target.name.clone(),
        percentage: None,
        bucket: None,
        step_index: None,
        detail: None,
        error_code: None,
    }
}

fn build_rollout_reason(result: &RolloutResult) -> Reason {
    if result.step_index >= 0 {
        Reason {
            reason_type: "gradual_rollout".to_string(),
            step_index: Some(result.step_index),
            percentage: Some(result.matched_weight as f64 / 1000.0),
            bucket: Some(result.bucket_value),
            rule_id: None,
            rule_name: None,
            detail: None,
            error_code: None,
        }
    } else {
        Reason {
            reason_type: "percentage_rollout".to_string(),
            percentage: Some(result.matched_weight as f64 / 1000.0),
            bucket: Some(result.bucket_value),
            rule_id: None,
            rule_name: None,
            step_index: None,
            detail: None,
            error_code: None,
        }
    }
}

/// Evaluate a feature flag against the given context.
/// This is a pure function with no I/O.
pub fn evaluate_flag(
    flag: &SnapshotFlag,
    context: &EvaluationContext,
    segments: &HashMap<String, SnapshotSegment>,
    now: Option<DateTime<Utc>>,
) -> EvalOutput {
    // Disabled flag → off variation
    if !flag.enabled {
        let value = flag.variations.get(&flag.off_variation_key)
            .map(|v| v.value.clone())
            .unwrap_or(serde_json::Value::Null);
        return EvalOutput {
            value,
            variation_key: Some(flag.off_variation_key.clone()),
            reasons: vec![Reason {
                reason_type: "off".to_string(),
                rule_id: None,
                rule_name: None,
                percentage: None,
                bucket: None,
                step_index: None,
                detail: None,
                error_code: None,
            }],
            matched_target_name: None,
            error_detail: None,
            inputs_used: vec![],
        };
    }

    let eval_now = now.unwrap_or_else(Utc::now);
    let mut inputs_used: Vec<String> = Vec::new();

    // Sort targets by sort_order
    let mut sorted_targets = flag.targets.clone();
    sorted_targets.sort_by_key(|t| t.sort_order);

    // Evaluate each target
    for target in &sorted_targets {
        if evaluate_target(target, context, segments, &mut inputs_used) {
            if let Some(resolved) = resolve_target_variation(target, &flag.key, context, &flag.variations, &mut inputs_used, eval_now) {
                let mut reasons = vec![build_rule_match_reason(target)];
                if target.rollout.is_some() {
                    reasons.push(build_rollout_reason(&resolved));
                }

                return EvalOutput {
                    value: resolved.variation.value.clone(),
                    variation_key: Some(resolved.variation_key),
                    reasons,
                    matched_target_name: target.name.clone(),
                    error_detail: None,
                    inputs_used,
                };
            }
        }
    }

    // Default fallback
    if let Some(resolved) = resolve_default_variation(flag, context, &mut inputs_used, eval_now) {
        let mut reasons = Vec::new();
        if flag.default_rollout.is_some() {
            reasons.push(build_rollout_reason(&resolved));
        }
        reasons.push(Reason {
            reason_type: "default".to_string(),
            rule_id: None,
            rule_name: None,
            percentage: None,
            bucket: None,
            step_index: None,
            detail: None,
            error_code: None,
        });

        return EvalOutput {
            value: resolved.variation.value.clone(),
            variation_key: Some(resolved.variation_key),
            reasons,
            matched_target_name: None,
            error_detail: None,
            inputs_used,
        };
    }

    // Final fallback
    EvalOutput {
        value: serde_json::Value::Null,
        variation_key: None,
        reasons: vec![Reason {
            reason_type: "default".to_string(),
            rule_id: None,
            rule_name: None,
            percentage: None,
            bucket: None,
            step_index: None,
            detail: None,
            error_code: None,
        }],
        matched_target_name: None,
        error_detail: None,
        inputs_used,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::fs;
    use std::path::Path;

    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct TestFixture {
        description: String,
        tests: Vec<TestCase>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TestCase {
        name: String,
        flag: serde_json::Value,
        #[serde(default)]
        context: EvaluationContext,
        #[serde(default)]
        segments: HashMap<String, SnapshotSegment>,
        #[serde(default)]
        options: TestOptions,
        expected: TestExpected,
    }

    #[derive(Deserialize, Default)]
    struct TestOptions {
        #[serde(default)]
        now: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TestExpected {
        #[serde(default)]
        value: Option<serde_json::Value>,
        #[serde(default)]
        variation_key: Option<String>,
        #[serde(default)]
        reasons: Vec<serde_json::Map<String, serde_json::Value>>,
        #[serde(default)]
        matched_target_name: Option<String>,
        #[serde(default)]
        value_one_of: Option<Vec<serde_json::Value>>,
    }

    fn load_fixtures() -> Vec<(String, TestCase)> {
        let dir = Path::new("../gradual-sdk-spec/testdata/evaluator");
        let mut entries: Vec<_> = fs::read_dir(dir)
            .expect("Failed to read fixture directory")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "json"))
            .collect();
        entries.sort_by_key(|e| e.file_name());

        let mut cases = Vec::new();
        for entry in entries {
            let data = fs::read_to_string(entry.path())
                .unwrap_or_else(|_| panic!("Failed to read {:?}", entry.path()));
            let fixture: TestFixture = serde_json::from_str(&data)
                .unwrap_or_else(|e| panic!("Failed to parse {:?}: {}", entry.path(), e));
            let file_name = entry.file_name().to_string_lossy().to_string();
            for tc in fixture.tests {
                cases.push((file_name.clone(), tc));
            }
        }
        cases
    }

    fn reason_matches(
        actual: &serde_json::Map<String, serde_json::Value>,
        expected: &serde_json::Map<String, serde_json::Value>,
    ) -> bool {
        for (k, v) in expected {
            match actual.get(k) {
                Some(av) if av == v => {}
                _ => return false,
            }
        }
        true
    }

    #[test]
    fn test_evaluator_conformance() {
        let cases = load_fixtures();
        let mut failures = Vec::new();

        for (file, tc) in &cases {
            let test_id = format!("{}::{}", file, tc.name);
            let flag: SnapshotFlag = match serde_json::from_value(tc.flag.clone()) {
                Ok(f) => f,
                Err(e) => {
                    failures.push(format!("{}: failed to parse flag: {}", test_id, e));
                    continue;
                }
            };

            let now = tc.options.now.as_ref().and_then(|s| s.parse::<DateTime<Utc>>().ok());
            let result = evaluate_flag(&flag, &tc.context, &tc.segments, now);

            // Check value
            if let Some(expected_value) = &tc.expected.value {
                let result_value = &result.value;
                if result_value != expected_value {
                    failures.push(format!(
                        "{}: value: got {}, want {}",
                        test_id, result_value, expected_value
                    ));
                }
            }

            // Check variationKey
            if let Some(expected_vk) = &tc.expected.variation_key {
                let result_vk = result.variation_key.as_deref().unwrap_or("");
                if result_vk != expected_vk {
                    failures.push(format!(
                        "{}: variationKey: got {:?}, want {:?}",
                        test_id, result_vk, expected_vk
                    ));
                }
            }

            // Check reasons
            if !tc.expected.reasons.is_empty() {
                let actual_reasons: Vec<serde_json::Map<String, serde_json::Value>> =
                    result.reasons.iter().map(|r| {
                        let v = serde_json::to_value(r).unwrap();
                        match v {
                            serde_json::Value::Object(m) => m,
                            _ => serde_json::Map::new(),
                        }
                    }).collect();

                for exp_reason in &tc.expected.reasons {
                    let found = actual_reasons.iter().any(|ar| reason_matches(ar, exp_reason));
                    if !found {
                        failures.push(format!(
                            "{}: expected reason {:?} not found in {:?}",
                            test_id, exp_reason, actual_reasons
                        ));
                    }
                }
            }

            // Check matchedTargetName
            if let Some(expected_name) = &tc.expected.matched_target_name {
                let result_name = result.matched_target_name.as_deref().unwrap_or("");
                if result_name != expected_name {
                    failures.push(format!(
                        "{}: matchedTargetName: got {:?}, want {:?}",
                        test_id, result_name, expected_name
                    ));
                }
            }

            // Check valueOneOf
            if let Some(one_of) = &tc.expected.value_one_of {
                let found = one_of.iter().any(|v| v == &result.value);
                if !found {
                    failures.push(format!(
                        "{}: value {} not in {:?}",
                        test_id, result.value, one_of
                    ));
                }
            }
        }

        if !failures.is_empty() {
            panic!(
                "\n{} conformance test failure(s):\n  {}",
                failures.len(),
                failures.join("\n  ")
            );
        }

        eprintln!("All {} evaluator conformance tests passed", cases.len());
    }
}
