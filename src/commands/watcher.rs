use std::collections::BTreeSet;

use serde_json::json;

use crate::client::{JiraClient, JiraTransport, WriteEndpoint, ensure_empty_write_response};
use crate::commands::issue::{encoded_path, validate_issue, validate_mutation_issue_key};
use crate::commands::schema_violation;
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{
    AppliedIssueKey, AppliedWatcher, AppliedWatcherMutation, CountMeta, JiraWatcherList,
    MutationPlan, ValidationLevel, WatcherInput, WatcherItem,
};
use crate::output::SuccessEnvelope;

pub fn validate_watcher_input(input: &WatcherInput) -> Result<(), AppError> {
    validate_mutation_issue_key(&input.issue_key)?;
    if !valid_account_id(&input.account_id) {
        return Err(schema_violation(
            "account_id must be nonblank, bounded, and contain no control characters",
        ));
    }
    Ok(())
}

pub fn plan_watcher_add(input: WatcherInput) -> Result<MutationPlan, AppError> {
    plan_watcher(input, "issue.watcher.add", "add")
}

pub fn plan_watcher_remove(input: WatcherInput) -> Result<MutationPlan, AppError> {
    plan_watcher(input, "issue.watcher.remove", "remove")
}

fn plan_watcher(
    input: WatcherInput,
    operation: &'static str,
    action: &'static str,
) -> Result<MutationPlan, AppError> {
    validate_watcher_input(&input)?;
    Ok(MutationPlan::dry_run(
        operation,
        json!({"issue": input.issue_key, "account_id": input.account_id}),
        json!({"action": action}),
        ValidationLevel::NotApplicable,
        json!(input.account_id),
    ))
}

pub fn apply_watcher_add<T: JiraTransport>(
    client: &JiraClient<T>,
    plan: MutationPlan,
) -> Result<AppliedWatcherMutation, AppError> {
    let (issue_key, account_id) = plan_target(&plan)?;
    let path = encoded_path(&["rest", "api", "3", "issue", &issue_key, "watchers"])?;
    let response =
        client.jira_write(WriteEndpoint::AddWatcher, &path, &plan.into_wire_payload())?;
    ensure_empty_write_response(WriteEndpoint::AddWatcher, response)?;
    Ok(applied_watcher("issue.watcher.add", issue_key, account_id))
}

pub fn apply_watcher_remove<T: JiraTransport>(
    client: &JiraClient<T>,
    plan: MutationPlan,
) -> Result<AppliedWatcherMutation, AppError> {
    let (issue_key, account_id) = plan_target(&plan)?;
    let path = encoded_path(&["rest", "api", "3", "issue", &issue_key, "watchers"])?;
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("accountId", &account_id);
    let response = client.jira_write_empty(
        WriteEndpoint::RemoveWatcher,
        &format!("{path}?{}", query.finish()),
    )?;
    ensure_empty_write_response(WriteEndpoint::RemoveWatcher, response)?;
    Ok(applied_watcher(
        "issue.watcher.remove",
        issue_key,
        account_id,
    ))
}

pub fn issue_watchers<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
) -> Result<SuccessEnvelope<Vec<WatcherItem>, CountMeta>, AppError> {
    validate_issue(issue)?;
    let path = encoded_path(&["rest", "api", "3", "issue", issue, "watchers"])?;
    let response: JiraWatcherList = client.get_json_exact(&path, 200)?;
    let mut seen = BTreeSet::new();
    let data = response
        .watchers
        .into_iter()
        .map(WatcherItem::from)
        .map(|watcher| {
            if !valid_account_id(&watcher.account_id)
                || watcher.display_name.trim().is_empty()
                || watcher.display_name.len() > 4096
                || watcher.display_name.chars().any(char::is_control)
                || !seen.insert(watcher.account_id.clone())
            {
                return Err(invalid_watcher_response());
            }
            Ok(watcher)
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let count = data.len();
    Ok(SuccessEnvelope::with_meta(data, CountMeta { count }))
}

fn plan_target(plan: &MutationPlan) -> Result<(String, String), AppError> {
    let issue_key = plan
        .target
        .get("issue")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let account_id = plan
        .target
        .get("account_id")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    match (issue_key, account_id) {
        (Some(issue_key), Some(account_id)) => {
            validate_watcher_input(&WatcherInput {
                issue_key: issue_key.clone(),
                account_id: account_id.clone(),
            })?;
            Ok((issue_key, account_id))
        }
        _ => Err(schema_violation("watcher plan target is invalid")),
    }
}

fn applied_watcher(
    operation: &'static str,
    issue_key: String,
    account_id: String,
) -> AppliedWatcherMutation {
    AppliedWatcherMutation {
        operation,
        applied: true,
        issue: AppliedIssueKey { key: issue_key },
        watcher: AppliedWatcher { account_id },
    }
}

fn valid_account_id(account_id: &str) -> bool {
    !account_id.trim().is_empty()
        && account_id.len() <= 1024
        && !account_id.chars().any(char::is_control)
}

fn invalid_watcher_response() -> AppError {
    AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned invalid watcher data",
        RetrySafety::Safe,
    )
}
