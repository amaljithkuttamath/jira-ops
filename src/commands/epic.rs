use crate::client::{JiraClient, JiraTransport, WriteEndpoint, ensure_empty_write_response};
use crate::commands::issue::{apply_create_issue, plan_create_issue};
use crate::commands::issue::{encoded_path, validate_mutation_issue_key};
use crate::commands::{schema_violation, validate_confirmation, validate_confirmed_set};
use crate::error::AppError;
use crate::model::{
    AppliedCreateIssue, AppliedEpicMembership, CreateIssueInput, EpicMembershipInput,
    EpicRemoveInput, MutationPlan, ValidationLevel,
};

pub fn plan_epic_create<T: JiraTransport>(
    client: &JiraClient<T>,
    input: CreateIssueInput,
) -> Result<MutationPlan, AppError> {
    let mut plan = plan_create_issue(client, input)?;
    plan.operation = "epic.create";
    Ok(plan)
}
pub fn apply_epic_create<T: JiraTransport>(
    client: &JiraClient<T>,
    mut plan: MutationPlan,
) -> Result<AppliedCreateIssue, AppError> {
    plan.operation = "issue.create";
    let mut applied = apply_create_issue(client, plan)?;
    applied.operation = "epic.create";
    Ok(applied)
}
use serde_json::json;
use std::collections::BTreeSet;

pub fn validate_epic_membership(input: &EpicMembershipInput) -> Result<(), AppError> {
    validate_keys(&input.issue_keys)
}
pub fn validate_epic_remove(epic: &str, input: &EpicRemoveInput) -> Result<(), AppError> {
    validate_mutation_issue_key(epic)?;
    validate_keys(&input.issue_keys)?;
    validate_confirmation(&input.confirm_epic, epic, "confirm_epic")?;
    validate_confirmed_set(
        &input.confirm_issue_keys,
        &input.issue_keys,
        "confirm_issue_keys",
    )
}

pub fn plan_epic_add(epic: &str, input: EpicMembershipInput) -> Result<MutationPlan, AppError> {
    validate_mutation_issue_key(epic)?;
    validate_epic_membership(&input)?;
    Ok(MutationPlan::dry_run(
        "epic.add",
        json!({"epic":epic}),
        json!({"issue_keys":input.issue_keys,"notify_users":input.notify_users}),
        ValidationLevel::NotApplicable,
        json!({"issues":input.issue_keys}),
    ))
}
pub fn plan_epic_remove(epic: &str, input: EpicRemoveInput) -> Result<MutationPlan, AppError> {
    validate_epic_remove(epic, &input)?;
    Ok(MutationPlan::dry_run(
        "epic.remove",
        json!({"epic":epic}),
        json!({"issue_keys":input.issue_keys,"notify_users":input.notify_users}),
        ValidationLevel::NotApplicable,
        json!({"issues":input.issue_keys}),
    ))
}
pub fn apply_epic_add<T: JiraTransport>(
    client: &JiraClient<T>,
    epic: &str,
    notify: bool,
    plan: MutationPlan,
) -> Result<AppliedEpicMembership, AppError> {
    apply(
        client,
        WriteEndpoint::AddEpicIssues,
        "epic.add",
        epic,
        epic,
        notify,
        plan,
    )
}
pub fn apply_epic_remove<T: JiraTransport>(
    client: &JiraClient<T>,
    epic: &str,
    notify: bool,
    plan: MutationPlan,
) -> Result<AppliedEpicMembership, AppError> {
    apply(
        client,
        WriteEndpoint::RemoveEpicIssues,
        "epic.remove",
        epic,
        "none",
        notify,
        plan,
    )
}
fn apply<T: JiraTransport>(
    client: &JiraClient<T>,
    endpoint: WriteEndpoint,
    operation: &'static str,
    epic: &str,
    path_epic: &str,
    notify: bool,
    plan: MutationPlan,
) -> Result<AppliedEpicMembership, AppError> {
    validate_mutation_issue_key(epic)?;
    let keys = plan.changes["issue_keys"]
        .as_array()
        .ok_or_else(|| schema_violation("epic plan is invalid"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| schema_violation("epic plan is invalid"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let base = encoded_path(&["rest", "agile", "1.0", "epic", path_epic, "issue"])?;
    let response = client.jira_write(
        endpoint,
        &format!("{base}?notifyUsers={notify}"),
        &plan.into_wire_payload(),
    )?;
    ensure_empty_write_response(endpoint, response)?;
    Ok(AppliedEpicMembership {
        operation,
        applied: true,
        epic: epic.into(),
        issue_keys: keys,
    })
}
fn validate_keys(keys: &[String]) -> Result<(), AppError> {
    if keys.is_empty() || keys.len() > 50 {
        return Err(schema_violation(
            "issue_keys must contain 1 through 50 keys",
        ));
    }
    let mut seen = BTreeSet::new();
    for key in keys {
        validate_mutation_issue_key(key)?;
        if !seen.insert(key) {
            return Err(schema_violation("issue_keys must be unique"));
        }
    }
    Ok(())
}

pub fn epic_jql(project: &str, caller: Option<&str>) -> Result<String, AppError> {
    if project.is_empty()
        || project.len() > 10
        || !project
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(schema_violation(
            "project must be an uppercase Jira project key",
        ));
    }
    Ok(caller.filter(|value| !value.trim().is_empty()).map_or_else(
        || format!("project = {project} AND issuetype = Epic"),
        |jql| format!("project = {project} AND issuetype = Epic AND ({jql})"),
    ))
}
