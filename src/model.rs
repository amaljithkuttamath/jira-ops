use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::content::ContentInput;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssignmentInput {
    pub issue_key: String,
    #[schemars(required)]
    #[serde(deserialize_with = "deserialize_nullable_string")]
    pub account_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkInput {
    pub inward_issue: String,
    pub outward_issue: String,
    pub type_name: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteIssueInput {
    pub confirm_issue: String,
    #[serde(default)]
    pub cascade: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveLinkInput {
    pub confirm_link_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedLinkRemoval {
    pub operation: &'static str,
    pub applied: bool,
    pub link_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteLinkInput {
    #[schemars(with = "String")]
    pub url: url::Url,
    pub title: String,
    #[serde(default)]
    pub relationship: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveRemoteLinkInput {
    pub confirm_remote_link_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RemoteLinkItem {
    pub id: u64,
    pub global_id: Option<String>,
    pub title: String,
    pub url: url::Url,
    pub relationship: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedRemoteLinkMutation {
    pub operation: &'static str,
    pub applied: bool,
    pub issue: AppliedIssueKey,
    pub remote_link_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraRemoteLink {
    pub id: u64,
    #[serde(default)]
    pub global_id: Option<String>,
    #[serde(default)]
    pub relationship: Option<String>,
    pub object: JiraRemoteLinkObject,
}

#[derive(Debug, Deserialize)]
pub struct JiraRemoteLinkObject {
    pub url: String,
    pub title: String,
}

#[derive(Debug, Deserialize)]
pub struct JiraRemoteLinkCreated {
    pub id: u64,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum EstimateAdjustment {
    #[default]
    Auto,
    Leave,
    New {
        new_estimate: String,
    },
    Manual {
        reduce_by: String,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorklogWriteInput {
    pub time_spent: String,
    #[serde(default)]
    pub started: Option<String>,
    #[serde(default)]
    pub comment: Option<ContentInput>,
    #[serde(default)]
    pub adjustment: EstimateAdjustment,
    #[serde(default = "default_true")]
    pub notify_users: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorklogDeleteInput {
    pub confirm_worklog_id: String,
    #[serde(default)]
    pub adjustment: EstimateAdjustment,
    #[serde(default = "default_true")]
    pub notify_users: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraWorklogPage {
    pub start_at: u64,
    pub total: u64,
    pub worklogs: Vec<JiraWorklog>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraWorklog {
    pub id: String,
    pub author: JiraAccount,
    pub started: String,
    pub time_spent: String,
    pub time_spent_seconds: u64,
    #[serde(default)]
    pub comment: Option<Value>,
    #[serde(default)]
    pub updated: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorklogItem {
    pub id: String,
    pub author: Account,
    pub started: String,
    pub time_spent: String,
    pub time_spent_seconds: u64,
    pub comment: Option<String>,
    pub updated: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedWorklogMutation {
    pub operation: &'static str,
    pub applied: bool,
    pub issue: AppliedIssueKey,
    pub worklog_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EpicMembershipInput {
    pub issue_keys: Vec<String>,
    #[serde(default = "default_true")]
    pub notify_users: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EpicRemoveInput {
    pub issue_keys: Vec<String>,
    pub confirm_epic: String,
    pub confirm_issue_keys: Vec<String>,
    #[serde(default = "default_true")]
    pub notify_users: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedEpicMembership {
    pub operation: &'static str,
    pub applied: bool,
    pub epic: String,
    pub issue_keys: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SprintState {
    Future,
    Active,
    Closed,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SprintAddInput {
    pub issue_keys: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SprintCloseInput {
    pub confirm_sprint_id: u64,
    #[serde(default)]
    pub complete_date: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraSprintPage {
    pub start_at: u64,
    pub total: u64,
    pub values: Vec<JiraSprint>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraSprint {
    pub id: u64,
    pub name: String,
    pub state: SprintState,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub complete_date: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SprintItem {
    pub id: u64,
    pub name: String,
    pub state: SprintState,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub complete_date: Option<String>,
    pub goal: Option<String>,
}

impl From<JiraSprint> for SprintItem {
    fn from(v: JiraSprint) -> Self {
        Self {
            id: v.id,
            name: v.name,
            state: v.state,
            start_date: v.start_date,
            end_date: v.end_date,
            complete_date: v.complete_date,
            goal: v.goal,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedSprintMutation {
    pub operation: &'static str,
    pub applied: bool,
    pub sprint_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_keys: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WatcherInput {
    pub issue_key: String,
    pub account_id: String,
}

fn deserialize_nullable_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateIssueInput {
    pub project_key: String,
    pub issue_type_id: String,
    pub fields: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateIssueInput {
    pub set: BTreeMap<String, Value>,
    #[serde(default)]
    pub notify_users: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentInput {
    pub body: ContentInput,
    #[serde(default)]
    pub internal: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransitionInput {
    pub transition_id: String,
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
    #[serde(default)]
    pub comment: Option<ContentInput>,
    #[serde(default)]
    pub notify_users: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCreateInput {
    pub key: String,
    pub name: String,
    pub project_type_key: String,
    pub project_template_key: String,
    pub lead_account_id: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub assignee_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectCreatePlan {
    pub operation: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub body: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProjectDetail {
    pub id: String,
    pub key: String,
    pub name: String,
    #[serde(rename = "type")]
    pub project_type: String,
    pub style: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraProjectDetail {
    pub id: String,
    pub key: String,
    pub name: String,
    pub project_type_key: String,
    pub style: String,
}

impl From<JiraProjectDetail> for ProjectDetail {
    fn from(value: JiraProjectDetail) -> Self {
        Self {
            id: value.id,
            key: value.key,
            name: value.name,
            project_type: value.project_type_key,
            style: value.style,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProjectTemplate {
    pub name: &'static str,
    pub project_type_key: &'static str,
    pub project_template_key: &'static str,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreatedProject {
    #[serde(deserialize_with = "deserialize_string_or_u64")]
    pub id: String,
    pub key: String,
}

fn deserialize_string_or_u64<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::String(value) => Ok(value),
        Value::Number(value) if value.is_u64() => Ok(value.to_string()),
        _ => Err(serde::de::Error::custom(
            "expected a project ID as a string or unsigned integer",
        )),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedProjectCreate {
    pub operation: &'static str,
    pub outcome: crate::error::OperationOutcome,
    pub project: CreatedProject,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationLevel {
    Passed,
    Partial,
    NotApplicable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MutationValidation {
    pub local: ValidationLevel,
    pub metadata: ValidationLevel,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MutationPlan {
    pub operation: &'static str,
    pub applied: bool,
    pub target: Value,
    pub changes: Value,
    pub validation: MutationValidation,
    #[serde(skip)]
    wire_payload: Value,
}

impl MutationPlan {
    pub fn dry_run(
        operation: &'static str,
        target: Value,
        changes: Value,
        metadata: ValidationLevel,
        wire_payload: Value,
    ) -> Self {
        Self {
            operation,
            applied: false,
            target,
            changes,
            validation: MutationValidation {
                local: ValidationLevel::Passed,
                metadata,
            },
            wire_payload,
        }
    }

    pub fn wire_payload(&self) -> &Value {
        &self.wire_payload
    }

    pub fn into_wire_payload(self) -> Value {
        self.wire_payload
    }
}

#[derive(Debug, Deserialize)]
pub struct JiraCreateIssueResponse {
    pub id: String,
    pub key: String,
}

#[derive(Debug, Deserialize)]
pub struct JiraCommentResponse {
    pub id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedCreateIssue {
    pub operation: &'static str,
    pub applied: bool,
    pub issue: AppliedCreatedIssue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedCreatedIssue {
    pub id: String,
    pub key: String,
    pub url: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedIssueMutation {
    pub operation: &'static str,
    pub applied: bool,
    pub issue: AppliedIssueKey,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedIssueKey {
    pub key: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedAssignmentMutation {
    pub operation: &'static str,
    pub applied: bool,
    pub issue: AppliedIssueKey,
    pub assignment: AppliedAssignment,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedAssignment {
    pub account_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedLinkMutation {
    pub operation: &'static str,
    pub applied: bool,
    pub link: AppliedLink,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedLink {
    pub inward_issue: String,
    pub outward_issue: String,
    pub type_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraIssueLinkTypes {
    pub issue_link_types: Vec<JiraIssueLinkType>,
}

#[derive(Debug, Deserialize)]
pub struct JiraIssueLinkType {
    pub id: String,
    pub name: String,
    pub inward: String,
    pub outward: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LinkTypeItem {
    pub id: String,
    pub name: String,
    pub inward: String,
    pub outward: String,
}

impl From<JiraIssueLinkType> for LinkTypeItem {
    fn from(value: JiraIssueLinkType) -> Self {
        Self {
            id: value.id,
            name: value.name,
            inward: value.inward,
            outward: value.outward,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraIssueLink {
    pub id: String,
    #[serde(rename = "type")]
    pub link_type: JiraIssueLinkType,
    pub inward_issue: JiraLinkedIssue,
    pub outward_issue: JiraLinkedIssue,
}

#[derive(Debug, Deserialize)]
pub struct JiraLinkedIssue {
    pub key: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LinkItem {
    pub id: String,
    #[serde(rename = "type")]
    pub link_type: LinkTypeItem,
    pub inward_issue: LinkedIssue,
    pub outward_issue: LinkedIssue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LinkedIssue {
    pub key: String,
}

impl From<JiraIssueLink> for LinkItem {
    fn from(value: JiraIssueLink) -> Self {
        Self {
            id: value.id,
            link_type: LinkTypeItem::from(value.link_type),
            inward_issue: LinkedIssue {
                key: value.inward_issue.key,
            },
            outward_issue: LinkedIssue {
                key: value.outward_issue.key,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedWatcherMutation {
    pub operation: &'static str,
    pub applied: bool,
    pub issue: AppliedIssueKey,
    pub watcher: AppliedWatcher,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedWatcher {
    pub account_id: String,
}

#[derive(Debug, Deserialize)]
pub struct JiraWatcherList {
    pub watchers: Vec<JiraWatcher>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraWatcher {
    pub account_id: String,
    pub display_name: String,
    pub active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WatcherItem {
    pub account_id: String,
    pub display_name: String,
    pub active: bool,
}

impl From<JiraWatcher> for WatcherItem {
    fn from(value: JiraWatcher) -> Self {
        Self {
            account_id: value.account_id,
            display_name: value.display_name,
            active: value.active,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedCommentMutation {
    pub operation: &'static str,
    pub applied: bool,
    pub issue: AppliedIssueKey,
    pub comment: AppliedComment,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppliedComment {
    pub id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PageMeta {
    pub count: usize,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CountMeta {
    pub count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProjectItem {
    pub id: String,
    pub key: String,
    pub name: String,
    pub project_type: String,
    pub simplified: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FieldItem {
    pub id: String,
    pub name: String,
    pub custom: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<FieldSchema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FieldSchema {
    #[serde(rename = "type")]
    pub value_type: Option<String>,
    pub items: Option<String>,
    pub custom: Option<String>,
    pub system: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraPage<T> {
    pub start_at: u64,
    pub total: u64,
    pub is_last: Option<bool>,
    pub values: Vec<T>,
    #[serde(default)]
    pub warning_messages: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraProject {
    pub id: String,
    pub key: String,
    pub name: String,
    pub project_type_key: String,
    pub simplified: bool,
}

impl From<JiraProject> for ProjectItem {
    fn from(value: JiraProject) -> Self {
        Self {
            id: value.id,
            key: value.key,
            name: value.name,
            project_type: value.project_type_key,
            simplified: value.simplified,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct JiraField {
    pub id: String,
    pub name: String,
    pub schema: Option<JiraFieldSchema>,
}

#[derive(Debug, Deserialize)]
pub struct JiraFieldSchema {
    #[serde(rename = "type")]
    pub value_type: Option<String>,
    pub items: Option<String>,
    pub custom: Option<String>,
    pub system: Option<String>,
}

impl From<JiraField> for FieldItem {
    fn from(value: JiraField) -> Self {
        let custom = value.id.starts_with("customfield_");
        Self {
            id: value.id,
            name: value.name,
            custom,
            schema: value.schema.map(|schema| FieldSchema {
                value_type: schema.value_type,
                items: schema.items,
                custom: schema.custom,
                system: schema.system,
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraAccount {
    pub account_id: String,
    pub display_name: String,
    pub active: bool,
    pub email_address: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Account {
    pub account_id: String,
    pub display_name: String,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ServerInfo {
    pub version: String,
    pub deployment_type: String,
    pub build_number: u64,
    pub build_date: String,
    pub server_time: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraServerInfo {
    pub version: String,
    pub deployment_type: String,
    pub build_number: u64,
    pub build_date: String,
    pub server_time: String,
}

impl From<JiraServerInfo> for ServerInfo {
    fn from(value: JiraServerInfo) -> Self {
        Self {
            version: value.version,
            deployment_type: value.deployment_type,
            build_number: value.build_number,
            build_date: value.build_date,
            server_time: value.server_time,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UserItem {
    pub account_id: String,
    pub display_name: String,
    pub active: bool,
    pub account_type: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraUserItem {
    pub account_id: String,
    pub display_name: String,
    pub active: bool,
    pub account_type: String,
}

impl From<JiraUserItem> for UserItem {
    fn from(value: JiraUserItem) -> Self {
        Self {
            account_id: value.account_id,
            display_name: value.display_name,
            active: value.active,
            account_type: value.account_type,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BoardItem {
    pub id: u64,
    pub name: String,
    #[serde(rename = "type")]
    pub board_type: String,
    pub project_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JiraBoardItem {
    pub id: u64,
    pub name: String,
    #[serde(rename = "type")]
    pub board_type: String,
    pub location: Option<JiraBoardLocation>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraBoardLocation {
    pub project_key: Option<String>,
}

impl From<JiraBoardItem> for BoardItem {
    fn from(value: JiraBoardItem) -> Self {
        Self {
            id: value.id,
            name: value.name,
            board_type: value.board_type,
            project_key: value.location.and_then(|location| location.project_key),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraBoardPage {
    pub max_results: u64,
    pub start_at: u64,
    pub total: u64,
    pub is_last: bool,
    pub values: Vec<JiraBoardItem>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReleaseItem {
    pub id: String,
    pub name: String,
    pub archived: bool,
    pub released: bool,
    pub start_date: Option<String>,
    pub release_date: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraReleaseItem {
    pub id: String,
    pub name: String,
    pub archived: bool,
    pub released: bool,
    pub start_date: Option<String>,
    pub release_date: Option<String>,
}

impl From<JiraReleaseItem> for ReleaseItem {
    fn from(value: JiraReleaseItem) -> Self {
        Self {
            id: value.id,
            name: value.name,
            archived: value.archived,
            released: value.released,
            start_date: value.start_date,
            release_date: value.release_date,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraReleasePage {
    pub start_at: u64,
    pub max_results: u64,
    pub total: u64,
    pub is_last: bool,
    pub values: Vec<JiraReleaseItem>,
}

impl From<JiraAccount> for Account {
    fn from(value: JiraAccount) -> Self {
        Self {
            account_id: value.account_id,
            display_name: value.display_name,
            active: value.active,
            email: value.email_address,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct JiraIssue {
    pub id: String,
    pub key: String,
    pub fields: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
pub struct JiraIssueStatus {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraIssueAssignee {
    pub account_id: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IssueAssignee {
    pub account_id: String,
    pub display_name: String,
}

impl From<JiraIssueAssignee> for IssueAssignee {
    fn from(value: JiraIssueAssignee) -> Self {
        Self {
            account_id: value.account_id,
            display_name: value.display_name,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct IssueProjection {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<Option<IssueAssignee>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraIssueSearchPage {
    pub is_last: bool,
    #[serde(default)]
    pub next_page_token: Option<String>,
    pub issues: Vec<JiraIssue>,
    #[serde(default)]
    pub warning_messages: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CreateMetaItem {
    IssueType(CreateIssueTypeItem),
    Field(FieldMetadata),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CreateIssueTypeItem {
    pub id: String,
    pub name: String,
    pub subtask: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    String,
    Number,
    Boolean,
    Array,
    Object,
    AdfText,
    Passthrough,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FieldMetadata {
    pub id: String,
    pub name: String,
    pub required: bool,
    pub operations: Vec<String>,
    pub schema: FieldSchema,
    pub input_kind: InputKind,
    pub supported_selector_members: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_values: Option<Vec<Value>>,
    pub allowed_values_complete: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CreateMetaPageMeta {
    pub kind: &'static str,
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_type_id: Option<String>,
    pub count: usize,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraCreateIssueTypesPage {
    pub start_at: u64,
    pub total: u64,
    pub issue_types: Vec<JiraCreateIssueType>,
}

#[derive(Debug, Deserialize)]
pub struct JiraCreateIssueType {
    pub id: String,
    pub name: String,
    pub subtask: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraCreateFieldsPage {
    pub start_at: u64,
    pub total: u64,
    pub fields: Vec<JiraFieldMetadata>,
}

#[derive(Debug, Deserialize)]
pub struct JiraEditMeta {
    pub fields: BTreeMap<String, JiraFieldMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraFieldMetadata {
    pub field_id: Option<String>,
    pub key: Option<String>,
    pub name: String,
    pub required: bool,
    #[serde(default)]
    pub operations: Vec<String>,
    pub schema: Option<JiraFieldSchema>,
    pub allowed_values: Option<Vec<Value>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CommentItem {
    pub id: String,
    pub author: IssueAssignee,
    pub body: String,
    pub created: String,
    pub updated: String,
}

#[derive(Debug, Deserialize)]
pub struct JiraComment {
    pub id: String,
    pub author: JiraIssueAssignee,
    pub body: Value,
    pub created: String,
    pub updated: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraCommentPage {
    pub start_at: u64,
    pub total: u64,
    pub comments: Vec<JiraComment>,
    #[serde(default)]
    pub warning_messages: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TransitionItem {
    pub id: String,
    pub name: String,
    pub to: TransitionStatus,
    pub fields: Vec<FieldMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TransitionStatus {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct JiraTransitions {
    pub transitions: Vec<JiraTransition>,
}

#[derive(Debug, Deserialize)]
pub struct JiraTransition {
    pub id: String,
    pub name: String,
    pub to: JiraTransitionStatus,
    pub fields: Option<BTreeMap<String, JiraFieldMetadata>>,
}

#[derive(Debug, Deserialize)]
pub struct JiraTransitionStatus {
    pub id: String,
    pub name: String,
}
