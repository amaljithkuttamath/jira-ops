use serde_json::json;

use crate::client::{JiraClient, JiraTransport, WriteEndpoint, ensure_empty_write_response};
use crate::commands::issue::{encoded_path, validate_mutation_issue_key};
use crate::commands::schema_violation;
use crate::error::AppError;
use crate::model::{
    AppliedAssignment, AppliedAssignmentMutation, AppliedIssueKey, AssignmentInput, MutationPlan,
    ValidationLevel,
};

pub fn validate_assignment_input(input: &AssignmentInput) -> Result<(), AppError> {
    validate_mutation_issue_key(&input.issue_key)?;
    if input
        .account_id
        .as_deref()
        .is_some_and(|account_id| !valid_account_id(account_id))
    {
        return Err(schema_violation(
            "account_id must be nonblank, bounded, and contain no control characters",
        ));
    }
    Ok(())
}

pub fn plan_assignment(input: AssignmentInput) -> Result<MutationPlan, AppError> {
    validate_assignment_input(&input)?;
    Ok(MutationPlan::dry_run(
        "issue.assign",
        json!({"issue": input.issue_key}),
        json!({"account_id": input.account_id}),
        ValidationLevel::NotApplicable,
        json!({"accountId": input.account_id}),
    ))
}

pub fn apply_assignment<T: JiraTransport>(
    client: &JiraClient<T>,
    plan: MutationPlan,
) -> Result<AppliedAssignmentMutation, AppError> {
    let issue_key = plan
        .target
        .get("issue")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| schema_violation("assignment plan target is invalid"))?
        .to_owned();
    let account_id = plan
        .changes
        .get("account_id")
        .and_then(|value| {
            if value.is_null() {
                Some(None)
            } else {
                value.as_str().map(|value| Some(value.to_owned()))
            }
        })
        .ok_or_else(|| schema_violation("assignment plan account is invalid"))?;
    validate_assignment_input(&AssignmentInput {
        issue_key: issue_key.clone(),
        account_id: account_id.clone(),
    })?;
    let path = encoded_path(&["rest", "api", "3", "issue", &issue_key, "assignee"])?;
    let response =
        client.jira_write(WriteEndpoint::AssignIssue, &path, &plan.into_wire_payload())?;
    ensure_empty_write_response(WriteEndpoint::AssignIssue, response)?;
    Ok(AppliedAssignmentMutation {
        operation: "issue.assign",
        applied: true,
        issue: AppliedIssueKey { key: issue_key },
        assignment: AppliedAssignment { account_id },
    })
}

fn valid_account_id(account_id: &str) -> bool {
    !account_id.trim().is_empty()
        && account_id.len() <= 1024
        && !account_id.chars().any(char::is_control)
}
