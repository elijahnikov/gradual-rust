use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type EvaluationContext = HashMap<String, HashMap<String, serde_json::Value>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetingOperator {
    Equals,
    NotEquals,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    GreaterThan,
    LessThan,
    GreaterThanOrEqual,
    LessThanOrEqual,
    In,
    NotIn,
    Exists,
    NotExists,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRuleCondition {
    pub context_kind: String,
    pub attribute_key: String,
    pub operator: TargetingOperator,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotIndividualEntry {
    pub context_kind: String,
    pub attribute_key: String,
    pub attribute_value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotSegment {
    pub key: String,
    pub conditions: Vec<SnapshotRuleCondition>,
    #[serde(default)]
    pub included: Vec<SnapshotIndividualEntry>,
    #[serde(default)]
    pub excluded: Vec<SnapshotIndividualEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRolloutVariation {
    pub variation_key: String,
    pub weight: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotScheduleStep {
    pub duration_minutes: i32,
    pub variations: Vec<SnapshotRolloutVariation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRollout {
    pub variations: Vec<SnapshotRolloutVariation>,
    pub bucket_context_kind: String,
    pub bucket_attribute_key: String,
    #[serde(default)]
    pub seed: Option<String>,
    #[serde(default)]
    pub schedule: Option<Vec<SnapshotScheduleStep>>,
    #[serde(default)]
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotTarget {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub target_type: String,
    pub sort_order: i32,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub variation_key: Option<String>,
    #[serde(default)]
    pub rollout: Option<SnapshotRollout>,
    #[serde(default)]
    pub conditions: Option<Vec<SnapshotRuleCondition>>,
    #[serde(default)]
    pub context_kind: Option<String>,
    #[serde(default)]
    pub attribute_key: Option<String>,
    #[serde(default)]
    pub attribute_value: Option<String>,
    #[serde(default)]
    pub segment_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotVariation {
    pub key: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotFlag {
    pub key: String,
    #[serde(rename = "type")]
    pub flag_type: String,
    pub enabled: bool,
    pub variations: HashMap<String, SnapshotVariation>,
    pub off_variation_key: String,
    pub targets: Vec<SnapshotTarget>,
    #[serde(default)]
    pub default_variation_key: Option<String>,
    #[serde(default)]
    pub default_rollout: Option<SnapshotRollout>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSnapshotMeta {
    pub project_id: String,
    pub organization_id: String,
    pub environment_slug: String,
    pub environment_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSnapshot {
    pub version: i32,
    pub generated_at: String,
    pub meta: EnvironmentSnapshotMeta,
    pub flags: HashMap<String, SnapshotFlag>,
    pub segments: HashMap<String, SnapshotSegment>,
}

#[derive(Debug, Clone)]
pub struct Reason {
    pub reason_type: String,
    pub rule_id: Option<String>,
    pub rule_name: Option<String>,
    pub percentage: Option<f64>,
    pub bucket: Option<i32>,
    pub step_index: Option<i32>,
    pub detail: Option<String>,
    pub error_code: Option<String>,
}

impl Serialize for Reason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("type", &self.reason_type)?;
        match self.reason_type.as_str() {
            "rule_match" => {
                if let Some(ref id) = self.rule_id {
                    map.serialize_entry("ruleId", id)?;
                }
                if let Some(ref name) = self.rule_name {
                    map.serialize_entry("ruleName", name)?;
                }
            }
            "percentage_rollout" => {
                if let Some(p) = self.percentage {
                    map.serialize_entry("percentage", &p)?;
                }
                if let Some(b) = self.bucket {
                    map.serialize_entry("bucket", &b)?;
                }
            }
            "gradual_rollout" => {
                if let Some(si) = self.step_index {
                    map.serialize_entry("stepIndex", &si)?;
                }
                if let Some(p) = self.percentage {
                    map.serialize_entry("percentage", &p)?;
                }
                if let Some(b) = self.bucket {
                    map.serialize_entry("bucket", &b)?;
                }
            }
            "error" => {
                if let Some(ref d) = self.detail {
                    map.serialize_entry("detail", d)?;
                }
                if let Some(ref ec) = self.error_code {
                    map.serialize_entry("errorCode", ec)?;
                }
            }
            _ => {}
        }
        map.end()
    }
}

#[derive(Debug, Clone)]
pub struct EvalOutput {
    pub value: serde_json::Value,
    pub variation_key: Option<String>,
    pub reasons: Vec<Reason>,
    pub matched_target_name: Option<String>,
    pub error_detail: Option<String>,
    pub inputs_used: Vec<String>,
}
