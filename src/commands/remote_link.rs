use serde_json::json;
use url::Url;

use crate::client::{
    JiraClient, JiraTransport, WriteEndpoint, decode_write_response, ensure_empty_write_response,
    invalid_write_success,
};
use crate::commands::issue::{encoded_path, validate_mutation_issue_key};
use crate::commands::{schema_violation, validate_confirmation};
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{
    AppliedIssueKey, AppliedRemoteLinkMutation, CountMeta, JiraRemoteLink, JiraRemoteLinkCreated,
    MutationPlan, RemoteLinkInput, RemoteLinkItem, RemoveRemoteLinkInput, ValidationLevel,
};
use crate::output::SuccessEnvelope;

const MAX_URL: usize = 2048;
const MAX_TITLE: usize = 255;
const MAX_RELATIONSHIP: usize = 255;

pub fn validate_remote_link_input(input: &RemoteLinkInput) -> Result<(), AppError> {
    validate_https_url(&input.url)?;
    validate_text(&input.title, MAX_TITLE, "title")?;
    if let Some(value) = &input.relationship {
        validate_text(value, MAX_RELATIONSHIP, "relationship")?;
    }
    Ok(())
}

pub fn remote_link_list<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
) -> Result<SuccessEnvelope<Vec<RemoteLinkItem>, CountMeta>, AppError> {
    validate_mutation_issue_key(issue)?;
    let path = encoded_path(&["rest", "api", "3", "issue", issue, "remotelink"])?;
    let response: Vec<JiraRemoteLink> = client.get_json_exact(&path, 200)?;
    if response.len() > 10_000 {
        return Err(invalid_response());
    }
    let data = response
        .into_iter()
        .map(project)
        .collect::<Result<Vec<_>, _>>()?;
    let count = data.len();
    Ok(SuccessEnvelope::with_meta(data, CountMeta { count }))
}

pub fn remote_link_get<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    link_id: &str,
) -> Result<SuccessEnvelope<RemoteLinkItem>, AppError> {
    validate_mutation_issue_key(issue)?;
    let id = validate_id(link_id)?;
    let path = encoded_path(&["rest", "api", "3", "issue", issue, "remotelink", link_id])?;
    let response: JiraRemoteLink = client.get_json_exact(&path, 200)?;
    let item = project(response)?;
    if item.id != id {
        return Err(invalid_response());
    }
    Ok(SuccessEnvelope::new(item))
}

pub fn plan_remote_link_add(issue: &str, input: RemoteLinkInput) -> Result<MutationPlan, AppError> {
    validate_mutation_issue_key(issue)?;
    validate_remote_link_input(&input)?;
    let mut body = json!({"object":{"url":input.url,"title":input.title}});
    if let Some(relationship) = input.relationship {
        body["relationship"] = json!(relationship);
    }
    Ok(MutationPlan::dry_run(
        "issue.remote-link.add",
        json!({"issue":issue}),
        body.clone(),
        ValidationLevel::NotApplicable,
        body,
    ))
}

pub fn apply_remote_link_add<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    plan: MutationPlan,
) -> Result<AppliedRemoteLinkMutation, AppError> {
    validate_mutation_issue_key(issue)?;
    let path = encoded_path(&["rest", "api", "3", "issue", issue, "remotelink"])?;
    let response = client.jira_write(
        WriteEndpoint::AddRemoteLink,
        &path,
        &plan.into_wire_payload(),
    )?;
    let status = response.status;
    let created: JiraRemoteLinkCreated =
        decode_write_response(WriteEndpoint::AddRemoteLink, response)?;
    if created.id == 0 {
        return Err(invalid_write_success(WriteEndpoint::AddRemoteLink, status));
    }
    Ok(AppliedRemoteLinkMutation {
        operation: "issue.remote-link.add",
        applied: true,
        issue: AppliedIssueKey { key: issue.into() },
        remote_link_id: created.id,
    })
}

pub fn plan_remote_link_remove<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    link_id: &str,
    input: RemoveRemoteLinkInput,
) -> Result<MutationPlan, AppError> {
    validate_mutation_issue_key(issue)?;
    validate_id(link_id)?;
    validate_confirmation(
        &input.confirm_remote_link_id,
        link_id,
        "confirm_remote_link_id",
    )?;
    let item = remote_link_get(client, issue, link_id)?.data;
    Ok(MutationPlan::dry_run(
        "issue.remote-link.remove",
        json!({"issue":issue,"remote_link_id":item.id}),
        json!({"title":item.title,"url":item.url}),
        ValidationLevel::Passed,
        json!(null),
    ))
}

pub fn apply_remote_link_remove<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    link_id: &str,
    _plan: MutationPlan,
) -> Result<AppliedRemoteLinkMutation, AppError> {
    validate_mutation_issue_key(issue)?;
    let id = validate_id(link_id)?;
    let path = encoded_path(&["rest", "api", "3", "issue", issue, "remotelink", link_id])?;
    let response = client.jira_write_empty(WriteEndpoint::RemoveRemoteLink, &path)?;
    ensure_empty_write_response(WriteEndpoint::RemoveRemoteLink, response)?;
    Ok(AppliedRemoteLinkMutation {
        operation: "issue.remote-link.remove",
        applied: true,
        issue: AppliedIssueKey { key: issue.into() },
        remote_link_id: id,
    })
}

fn project(value: JiraRemoteLink) -> Result<RemoteLinkItem, AppError> {
    let url = Url::parse(&value.object.url).map_err(|_| invalid_response())?;
    validate_https_url(&url).map_err(|_| invalid_response())?;
    validate_text(&value.object.title, MAX_TITLE, "title").map_err(|_| invalid_response())?;
    if let Some(v) = &value.relationship {
        validate_text(v, MAX_RELATIONSHIP, "relationship").map_err(|_| invalid_response())?;
    }
    if value
        .global_id
        .as_ref()
        .is_some_and(|v| validate_text(v, 1024, "global_id").is_err())
        || value.id == 0
    {
        return Err(invalid_response());
    }
    Ok(RemoteLinkItem {
        id: value.id,
        global_id: value.global_id,
        title: value.object.title,
        url,
        relationship: value.relationship,
    })
}
fn validate_https_url(url: &Url) -> Result<(), AppError> {
    if url.scheme() == "https"
        && url.host_str().is_some()
        && url.as_str().len() <= MAX_URL
        && url.username().is_empty()
        && url.password().is_none()
    {
        Ok(())
    } else {
        Err(schema_violation(
            "remote link url must be a bounded HTTPS URL without credentials",
        ))
    }
}
fn validate_text(value: &str, max: usize, field: &str) -> Result<(), AppError> {
    if !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(schema_violation(format!(
            "{field} must be nonblank, bounded, and contain no control characters"
        )))
    }
}
fn validate_id(value: &str) -> Result<u64, AppError> {
    value
        .parse::<u64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| schema_violation("remote link ID must be a positive decimal integer"))
}
fn invalid_response() -> AppError {
    AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned invalid remote link data",
        RetrySafety::Safe,
    )
}
