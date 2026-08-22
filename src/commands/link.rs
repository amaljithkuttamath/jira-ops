use std::collections::BTreeSet;

use serde_json::json;

use crate::client::{JiraClient, JiraTransport, WriteEndpoint, ensure_empty_write_response};
use crate::commands::issue::{encoded_path, validate_mutation_issue_key};
use crate::commands::{schema_violation, validate_confirmation};
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{
    AppliedLink, AppliedLinkMutation, AppliedLinkRemoval, CountMeta, JiraIssueLink,
    JiraIssueLinkTypes, LinkInput, LinkItem, LinkTypeItem, MutationPlan, RemoveLinkInput,
    ValidationLevel,
};
use crate::output::SuccessEnvelope;

pub fn validate_link_input(input: &LinkInput) -> Result<(), AppError> {
    validate_mutation_issue_key(&input.inward_issue)?;
    validate_mutation_issue_key(&input.outward_issue)?;
    if !valid_text_identifier(&input.type_name, 255) {
        return Err(schema_violation(
            "type_name must be nonblank, bounded, and contain no control characters",
        ));
    }
    Ok(())
}

pub fn plan_link(input: LinkInput) -> Result<MutationPlan, AppError> {
    validate_link_input(&input)?;
    Ok(MutationPlan::dry_run(
        "issue.link.add",
        json!({
            "inward_issue": input.inward_issue,
            "outward_issue": input.outward_issue
        }),
        json!({"type_name": input.type_name}),
        ValidationLevel::NotApplicable,
        json!({
            "type": {"name": input.type_name},
            "inwardIssue": {"key": input.inward_issue},
            "outwardIssue": {"key": input.outward_issue}
        }),
    ))
}

pub fn apply_link<T: JiraTransport>(
    client: &JiraClient<T>,
    plan: MutationPlan,
) -> Result<AppliedLinkMutation, AppError> {
    let inward_issue = plan_string(&plan.target, "inward_issue")?;
    let outward_issue = plan_string(&plan.target, "outward_issue")?;
    let type_name = plan_string(&plan.changes, "type_name")?;
    validate_link_input(&LinkInput {
        inward_issue: inward_issue.clone(),
        outward_issue: outward_issue.clone(),
        type_name: type_name.clone(),
    })?;
    let response = client.jira_write(
        WriteEndpoint::AddIssueLink,
        "/rest/api/3/issueLink",
        &plan.into_wire_payload(),
    )?;
    ensure_empty_write_response(WriteEndpoint::AddIssueLink, response)?;
    Ok(AppliedLinkMutation {
        operation: "issue.link.add",
        applied: true,
        link: AppliedLink {
            inward_issue,
            outward_issue,
            type_name,
        },
    })
}

pub fn issue_link_types<T: JiraTransport>(
    client: &JiraClient<T>,
) -> Result<SuccessEnvelope<Vec<LinkTypeItem>, CountMeta>, AppError> {
    let response: JiraIssueLinkTypes = client.get_json_exact("/rest/api/3/issueLinkType", 200)?;
    let mut seen = BTreeSet::new();
    let data = response
        .issue_link_types
        .into_iter()
        .map(LinkTypeItem::from)
        .map(|item| {
            validate_link_type(&item)?;
            if !seen.insert(item.id.clone()) {
                return Err(invalid_link_response());
            }
            Ok(item)
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let count = data.len();
    Ok(SuccessEnvelope::with_meta(data, CountMeta { count }))
}

pub fn issue_link_get<T: JiraTransport>(
    client: &JiraClient<T>,
    link_id: &str,
) -> Result<SuccessEnvelope<LinkItem>, AppError> {
    validate_link_id(link_id)?;
    let path = encoded_path(&["rest", "api", "3", "issueLink", link_id])?;
    let response: JiraIssueLink = client.get_json_exact(&path, 200)?;
    let item = LinkItem::from(response);
    if item.id != link_id
        || validate_link_type(&item.link_type).is_err()
        || validate_mutation_issue_key(&item.inward_issue.key).is_err()
        || validate_mutation_issue_key(&item.outward_issue.key).is_err()
    {
        return Err(invalid_link_response());
    }
    Ok(SuccessEnvelope::new(item))
}

pub fn plan_remove_link<T: JiraTransport>(
    client: &JiraClient<T>,
    link_id: &str,
    input: RemoveLinkInput,
) -> Result<MutationPlan, AppError> {
    validate_link_id(link_id)?;
    validate_confirmation(&input.confirm_link_id, link_id, "confirm_link_id")?;
    let link = issue_link_get(client, link_id)?.data;
    Ok(MutationPlan::dry_run(
        "issue.link.remove",
        json!({
            "link_id":link.id,
            "inward_issue":link.inward_issue.key,
            "outward_issue":link.outward_issue.key
        }),
        json!({"type_name":link.link_type.name}),
        ValidationLevel::Passed,
        json!(null),
    ))
}

pub fn apply_remove_link<T: JiraTransport>(
    client: &JiraClient<T>,
    link_id: &str,
    _plan: MutationPlan,
) -> Result<AppliedLinkRemoval, AppError> {
    validate_link_id(link_id)?;
    let path = encoded_path(&["rest", "api", "3", "issueLink", link_id])?;
    let response = client.jira_write_empty(WriteEndpoint::RemoveIssueLink, &path)?;
    ensure_empty_write_response(WriteEndpoint::RemoveIssueLink, response)?;
    Ok(AppliedLinkRemoval {
        operation: "issue.link.remove",
        applied: true,
        link_id: link_id.to_owned(),
    })
}

fn validate_link_id(link_id: &str) -> Result<(), AppError> {
    if link_id
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(byte, b'1'..=b'9'))
        && link_id.bytes().all(|byte| byte.is_ascii_digit())
        && link_id.len() <= 64
    {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::InvalidInput,
            "the issue link identifier must be a positive decimal ID",
            RetrySafety::Safe,
        ))
    }
}

fn validate_link_type(link_type: &LinkTypeItem) -> Result<(), AppError> {
    if [
        link_type.id.as_str(),
        link_type.name.as_str(),
        link_type.inward.as_str(),
        link_type.outward.as_str(),
    ]
    .into_iter()
    .all(|value| valid_text_identifier(value, 1024))
    {
        Ok(())
    } else {
        Err(invalid_link_response())
    }
}

fn valid_text_identifier(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn plan_string(value: &serde_json::Value, field: &str) -> Result<String, AppError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| schema_violation("issue link plan is invalid"))
}

fn invalid_link_response() -> AppError {
    AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned invalid issue link data",
        RetrySafety::Safe,
    )
}
