use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::adf::adf_to_text;
use crate::client::{JiraClient, JiraTransport};
use crate::commands::issue::{
    apply_create_issue, encoded_path, plan_create_issue, validate_mutation_issue_key,
};
use crate::commands::schema_violation;
use crate::error::AppError;
use crate::model::{AppliedCreateIssue, CreateIssueInput, MutationPlan};

const MAX_REPLACEMENTS: usize = 100;
const MAX_TEXT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloneIssueInput {
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub priority_id: Option<String>,
    #[serde(default)]
    pub assignee_account_id: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub component_ids: Option<Vec<String>>,
    #[serde(default)]
    pub replacements: Vec<ReplaceRule>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplaceRule {
    pub search: String,
    pub replacement: String,
}

#[derive(Debug, Deserialize)]
struct CloneSource {
    fields: CloneSourceFields,
}

#[derive(Debug, Deserialize)]
struct CloneSourceFields {
    project: CloneSelector,
    issuetype: CloneSelector,
    summary: String,
    description: Option<Value>,
    priority: Option<CloneSelector>,
    assignee: Option<CloneAssignee>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    components: Vec<CloneSelector>,
    parent: Option<CloneParent>,
}

#[derive(Debug, Deserialize)]
struct CloneSelector {
    id: Option<String>,
    key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloneAssignee {
    account_id: String,
}

#[derive(Debug, Deserialize)]
struct CloneParent {
    key: String,
}

pub fn plan_clone_issue<T: JiraTransport>(
    client: &JiraClient<T>,
    source_issue: &str,
    input: CloneIssueInput,
) -> Result<MutationPlan, AppError> {
    validate_mutation_issue_key(source_issue)?;
    validate_input(&input)?;
    let path = encoded_path(&["rest", "api", "3", "issue", source_issue])?;
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair(
        "fields",
        "project,issuetype,summary,description,priority,assignee,labels,components,parent",
    );
    let source: CloneSource = client.get_json(&format!("{path}?{}", query.finish()))?;

    let project_key = source
        .fields
        .project
        .key
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| schema_violation("clone source project is invalid"))?;
    let issue_type_id = source
        .fields
        .issuetype
        .id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| schema_violation("clone source issue type is invalid"))?;

    let mut fields = BTreeMap::new();
    let summary = apply_replacements(
        input.summary.as_deref().unwrap_or(&source.fields.summary),
        &input.replacements,
    )?;
    if summary.trim().is_empty() {
        return Err(schema_violation("clone summary must not be blank"));
    }
    fields.insert("summary".to_owned(), json!(summary));

    if let Some(description) = source.fields.description {
        if input.replacements.is_empty() {
            fields.insert(
                "description".to_owned(),
                json!({"format":"adf","value":description}),
            );
        } else {
            let text = adf_to_text(&description)?;
            fields.insert(
                "description".to_owned(),
                json!(apply_replacements(&text, &input.replacements)?),
            );
        }
    }

    let priority_id = input
        .priority_id
        .or_else(|| source.fields.priority.and_then(|priority| priority.id));
    if let Some(priority_id) = priority_id {
        fields.insert("priority".to_owned(), json!({"id":priority_id}));
    }
    let assignee = input
        .assignee_account_id
        .or_else(|| source.fields.assignee.map(|assignee| assignee.account_id));
    if let Some(account_id) = assignee {
        fields.insert("assignee".to_owned(), json!({"account_id":account_id}));
    }
    let labels = input.labels.unwrap_or(source.fields.labels);
    if !labels.is_empty() {
        fields.insert("labels".to_owned(), json!(labels));
    }
    let components = input.component_ids.map_or_else(
        || {
            source
                .fields
                .components
                .into_iter()
                .filter_map(|component| component.id)
                .collect()
        },
        |values| values,
    );
    if !components.is_empty() {
        fields.insert(
            "components".to_owned(),
            Value::Array(components.into_iter().map(|id| json!({"id":id})).collect()),
        );
    }
    let parent = input
        .parent
        .or_else(|| source.fields.parent.map(|parent| parent.key));
    if let Some(parent) = parent {
        validate_mutation_issue_key(&parent)?;
        fields.insert("parent".to_owned(), json!({"key":parent}));
    }

    let create_input = CreateIssueInput {
        project_key,
        issue_type_id,
        fields,
    };
    let mut plan = plan_create_issue(client, create_input)?;
    plan.operation = "issue.clone";
    plan.target = json!({"source_issue":source_issue});
    Ok(plan)
}

pub fn apply_clone_issue<T: JiraTransport>(
    client: &JiraClient<T>,
    plan: MutationPlan,
) -> Result<AppliedCreateIssue, AppError> {
    let mut applied = apply_create_issue(client, plan)?;
    applied.operation = "issue.clone";
    Ok(applied)
}

fn validate_input(input: &CloneIssueInput) -> Result<(), AppError> {
    if input.replacements.len() > MAX_REPLACEMENTS {
        return Err(schema_violation("clone replacement count exceeds 100"));
    }
    for rule in &input.replacements {
        if rule.search.is_empty() {
            return Err(schema_violation(
                "clone replacement search must not be empty",
            ));
        }
        validate_text(&rule.search)?;
        validate_text(&rule.replacement)?;
    }
    for value in input
        .summary
        .iter()
        .chain(input.priority_id.iter())
        .chain(input.assignee_account_id.iter())
        .chain(input.parent.iter())
    {
        validate_text(value)?;
    }
    Ok(())
}

fn apply_replacements(source: &str, rules: &[ReplaceRule]) -> Result<String, AppError> {
    validate_text(source)?;
    let mut value = source.to_owned();
    for rule in rules {
        value = value.replace(&rule.search, &rule.replacement);
        validate_text(&value)?;
    }
    Ok(value)
}

fn validate_text(value: &str) -> Result<(), AppError> {
    if value.len() > MAX_TEXT_BYTES
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(schema_violation("clone text is invalid or exceeds 1 MiB"));
    }
    Ok(())
}
