use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

mod epoch_millis {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Timestamp {
        Millis(i64),
        Rfc3339(String),
    }

    pub fn serialize<S>(value: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_i64(value.timestamp_millis())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        match Timestamp::deserialize(deserializer)? {
            Timestamp::Millis(value) => DateTime::from_timestamp_millis(value)
                .ok_or_else(|| D::Error::custom("goal timestamp is outside the supported range")),
            Timestamp::Rfc3339(value) => DateTime::parse_from_rfc3339(&value)
                .map(|value| value.with_timezone(&Utc))
                .map_err(D::Error::custom),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    #[serde(alias = "done")]
    Complete,
    Cleared,
}

impl GoalStatus {
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Blocked
                | Self::UsageLimited
                | Self::BudgetLimited
                | Self::Complete
                | Self::Cleared
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalVerdict {
    Done,
    Continue,
    Skipped,
    Blocked,
    Error,
    Inactive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoalCriterion {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub satisfied: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoalCompletionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoalCompletion {
    pub title: String,
    pub edit: GoalCompletionOption,
    pub new: GoalCompletionOption,
    pub clear: GoalCompletionOption,
    pub dismiss: GoalCompletionOption,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoalState {
    #[serde(rename = "goalID")]
    pub goal_id: String,
    pub goal: String,
    pub status: GoalStatus,
    #[serde(default)]
    pub tokens_used: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    /// The budget the goal was set with. Resuming an exhausted goal extends
    /// `token_budget` by this much; older goals use their current budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_allowance: Option<u64>,
    #[serde(default)]
    pub time_used_seconds: u64,
    #[serde(default)]
    pub turns_used: u32,
    #[serde(with = "epoch_millis")]
    #[schema(value_type = i64)]
    pub created_at: DateTime<Utc>,
    #[serde(with = "epoch_millis")]
    #[schema(value_type = i64)]
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verdict: Option<GoalVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_reason: Option<String>,
    /// Runtime circuit breakers set this only when the next ordinary user
    /// message should naturally resume the pursuit. Manual and budget pauses
    /// deliberately keep it false.
    #[serde(default)]
    pub awaiting_user_input: bool,
    #[serde(default)]
    pub consecutive_parse_failures: u32,
    #[serde(default)]
    pub consecutive_blocker_turns: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_blocker_reason: Option<String>,
    /// Logical goal-turn number on which the blocker recurrence was last counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_blocker_turn: Option<u32>,
    #[serde(default)]
    pub subgoals: Vec<String>,
    #[serde(default)]
    pub criteria: Vec<GoalCriterion>,
    #[serde(default)]
    pub needs_criteria: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum GoalActionRequest {
    Status,
    Pause,
    Resume,
    Clear,
    Complete,
    Block {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Set {
        goal: String,
        #[serde(
            rename = "tokenBudget",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        token_budget: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoalActionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<GoalState>,
    /// Compatibility mirror for clients that consumed the original `{ goal }` API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoalCriteriaEvidence {
    pub index: usize,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalUpdateStatus {
    Complete,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GoalUpdateRequest {
    pub status: GoalUpdateStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_step_question: Option<GoalCompletion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub criteria_evidence: Vec<GoalCriteriaEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoalUpdatedEvent {
    pub state: GoalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<GoalVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<GoalCompletion>,
}

/// Observable boundary for one provider-facing goal iteration. Haarness may
/// execute several logical goal turns inside one durable user turn; this event
/// preserves the lifecycle distinction without manufacturing transcript
/// messages or nested durable turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalLogicalTurnPhase {
    Started,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoalLogicalTurnEvent {
    #[serde(rename = "goalID")]
    pub goal_id: String,
    pub durable_turn_id: crate::TurnId,
    /// Zero is the user-triggered iteration; positive values are autonomous
    /// continuations already reflected by `GoalState::turns_used`.
    pub logical_turn: u32,
    pub continuation: bool,
    pub phase: GoalLogicalTurnPhase,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use serde_json::{json, to_value};

    use chrono::{TimeZone as _, Utc};

    use super::{GoalActionRequest, GoalCompletion, GoalState, GoalStatus};

    #[test]
    fn action_contract_is_tagged_and_camel_cases_fields() {
        let action: GoalActionRequest = serde_json::from_value(json!({
            "action": "set",
            "goal": "ship it",
            "tokenBudget": 12_000
        }))
        .unwrap();
        assert!(matches!(
            action,
            GoalActionRequest::Set {
                token_budget: Some(12_000),
                ..
            }
        ));
    }

    #[test]
    fn legacy_done_status_normalizes_on_deserialize() {
        let status: GoalStatus = serde_json::from_value(json!("done")).unwrap();
        assert_eq!(status, GoalStatus::Complete);
        assert_eq!(to_value(status).unwrap(), json!("complete"));
    }

    #[test]
    fn completion_contract_keeps_all_four_options() {
        let value = json!({
            "title": "What next?",
            "edit": {"label": "Refine", "description": "Adjust this goal."},
            "new": {"label": "New", "description": "Start another goal."},
            "clear": {"label": "Clear", "description": "Remove the goal."},
            "dismiss": {"label": "Done", "description": "Close this dialog."}
        });
        let completion: GoalCompletion = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(to_value(completion).unwrap(), value);
    }

    #[test]
    fn goal_state_uses_epoch_milliseconds_and_reads_legacy_rfc3339() {
        let state = GoalState {
            goal_id: "goal-1".into(),
            goal: "ship".into(),
            status: GoalStatus::Active,
            tokens_used: 0,
            token_budget: None,
            token_allowance: None,
            time_used_seconds: 0,
            turns_used: 0,
            created_at: Utc.timestamp_millis_opt(1_700_000_000_123).unwrap(),
            updated_at: Utc.timestamp_millis_opt(1_700_000_000_456).unwrap(),
            last_verdict: None,
            last_reason: None,
            paused_reason: None,
            awaiting_user_input: false,
            consecutive_parse_failures: 0,
            consecutive_blocker_turns: 0,
            last_blocker_reason: None,
            last_blocker_turn: None,
            subgoals: Vec::new(),
            criteria: Vec::new(),
            needs_criteria: false,
        };
        let value = to_value(&state).unwrap();
        assert_eq!(value["createdAt"], json!(1_700_000_000_123_i64));
        assert_eq!(value["updatedAt"], json!(1_700_000_000_456_i64));

        let mut legacy = value;
        legacy["createdAt"] = json!("2023-11-14T22:13:20.123Z");
        legacy["updatedAt"] = json!("2023-11-14T22:13:20.456Z");
        legacy.as_object_mut().unwrap().remove("awaitingUserInput");
        let restored: GoalState = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored.created_at.timestamp_millis(), 1_700_000_000_123);
        assert_eq!(restored.updated_at.timestamp_millis(), 1_700_000_000_456);
        assert!(!restored.awaiting_user_input);
    }
}
