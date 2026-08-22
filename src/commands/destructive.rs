use serde_json::json;

use crate::client::{JiraClient, JiraTransport, WriteEndpoint, ensure_empty_write_response};
use crate::commands::issue::{encoded_path, validate_mutation_issue_key};
use crate::commands::validate_confirmation;
use crate::error::AppError;
use crate::model::{
    AppliedIssueKey, AppliedIssueMutation, DeleteIssueInput, MutationPlan, ValidationLevel,
};

pub fn validate_delete_input(issue: &str, input: &DeleteIssueInput) -> Result<(), AppError> {
    validate_mutation_issue_key(issue)?;
    validate_confirmation(&input.confirm_issue, issue, "confirm_issue")
}

pub fn plan_delete_issue(issue: &str, input: DeleteIssueInput) -> Result<MutationPlan, AppError> {
    validate_delete_input(issue, &input)?;
    Ok(MutationPlan::dry_run(
        "issue.delete",
        json!({"issue":issue}),
        json!({"cascade":input.cascade}),
        ValidationLevel::NotApplicable,
        json!(null),
    ))
}

pub fn apply_delete_issue<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    plan: MutationPlan,
) -> Result<AppliedIssueMutation, AppError> {
    validate_mutation_issue_key(issue)?;
    let cascade = plan
        .changes
        .get("cascade")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let path = encoded_path(&["rest", "api", "3", "issue", issue])?;
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("deleteSubtasks", if cascade { "true" } else { "false" });
    let response = client.jira_write_empty(
        WriteEndpoint::DeleteIssue,
        &format!("{path}?{}", query.finish()),
    )?;
    ensure_empty_write_response(WriteEndpoint::DeleteIssue, response)?;
    Ok(AppliedIssueMutation {
        operation: "issue.delete",
        applied: true,
        issue: AppliedIssueKey {
            key: issue.to_owned(),
        },
    })
}
