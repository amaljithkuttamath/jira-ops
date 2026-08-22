use crate::client::{
    JiraClient, JiraTransport, WriteEndpoint, decode_write_response, ensure_empty_write_response,
    invalid_write_success,
};
use crate::commands::issue::encoded_path;
use crate::commands::{schema_violation, validate_confirmation};
use crate::cursor::{PageState, QueryFingerprint, decode_cursor, encode_cursor};
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{
    AppliedSprintMutation, JiraSprint, JiraSprintPage, MutationPlan, PageMeta, SprintAddInput,
    SprintCloseInput, SprintItem, SprintState, ValidationLevel,
};
use crate::output::SuccessEnvelope;
use serde_json::json;
use std::collections::BTreeSet;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub fn parse_sprint_state(value: Option<&str>) -> Result<Option<SprintState>, AppError> {
    value
        .map(|value| match value {
            "future" => Ok(SprintState::Future),
            "active" => Ok(SprintState::Active),
            "closed" => Ok(SprintState::Closed),
            _ => Err(schema_violation("state must be future, active, or closed")),
        })
        .transpose()
}

pub fn validate_sprint_add(input: &SprintAddInput) -> Result<(), AppError> {
    if input.issue_keys.is_empty() || input.issue_keys.len() > 50 {
        return Err(schema_violation(
            "issue_keys must contain 1 through 50 keys",
        ));
    }
    let mut seen = BTreeSet::new();
    for key in &input.issue_keys {
        crate::commands::issue::validate_mutation_issue_key(key)?;
        if !seen.insert(key) {
            return Err(schema_violation("issue_keys must be unique"));
        }
    }
    Ok(())
}
pub fn validate_sprint_close(id: u64, input: &SprintCloseInput) -> Result<(), AppError> {
    if id == 0 {
        return Err(schema_violation("sprint ID must be positive"));
    }
    validate_confirmation(
        &input.confirm_sprint_id.to_string(),
        &id.to_string(),
        "confirm_sprint_id",
    )?;
    if let Some(date) = &input.complete_date {
        OffsetDateTime::parse(date, &Rfc3339).map_err(|_| {
            schema_violation("complete_date must be RFC3339 with an explicit offset")
        })?;
    }
    Ok(())
}
pub fn sprint_list<T: JiraTransport>(
    client: &JiraClient<T>,
    board: u64,
    state: Option<SprintState>,
    limit: u16,
    cursor: Option<&str>,
) -> Result<SuccessEnvelope<Vec<SprintItem>, PageMeta>, AppError> {
    if board == 0 {
        return Err(schema_violation("board ID must be positive"));
    }
    let state_name = state.map(|v| match v {
        SprintState::Future => "future",
        SprintState::Active => "active",
        SprintState::Closed => "closed",
    });
    let fp = QueryFingerprint::new(&format!("board={board}&state={state_name:?}&limit={limit}"));
    let offset = cursor
        .map(|v| decode_cursor(v, "sprint.list", &fp))
        .transpose()?
        .map(|s| match s {
            PageState::Offset(v) => Ok(v),
            _ => Err(invalid()),
        })
        .transpose()?
        .unwrap_or(0);
    let base = encoded_path(&[
        "rest",
        "agile",
        "1.0",
        "board",
        &board.to_string(),
        "sprint",
    ])?;
    let mut q = url::form_urlencoded::Serializer::new(String::new());
    if let Some(s) = state_name {
        q.append_pair("state", s);
    }
    q.append_pair("startAt", &offset.to_string());
    q.append_pair("maxResults", &limit.to_string());
    let page: JiraSprintPage = client.get_json_exact(&format!("{base}?{}", q.finish()), 200)?;
    if page.start_at != offset || page.values.len() > usize::from(limit) {
        return Err(invalid());
    }
    let count = page.values.len();
    let next = offset.checked_add(count as u64).ok_or_else(invalid)?;
    let next_cursor = (count > 0 && next < page.total)
        .then(|| encode_cursor("sprint.list", &fp, PageState::Offset(next)))
        .transpose()?;
    let data = page.values.into_iter().map(SprintItem::from).collect();
    Ok(SuccessEnvelope::with_meta(
        data,
        PageMeta { count, next_cursor },
    ))
}
pub fn plan_sprint_add(id: u64, input: SprintAddInput) -> Result<MutationPlan, AppError> {
    if id == 0 {
        return Err(schema_violation("sprint ID must be positive"));
    }
    validate_sprint_add(&input)?;
    Ok(MutationPlan::dry_run(
        "sprint.add",
        json!({"sprint_id":id}),
        json!({"issue_keys":input.issue_keys}),
        ValidationLevel::NotApplicable,
        json!({"issues":input.issue_keys}),
    ))
}
pub fn apply_sprint_add<T: JiraTransport>(
    client: &JiraClient<T>,
    id: u64,
    plan: MutationPlan,
) -> Result<AppliedSprintMutation, AppError> {
    let keys = plan.changes["issue_keys"]
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let base = encoded_path(&["rest", "agile", "1.0", "sprint", &id.to_string(), "issue"])?;
    let response = client.jira_write(
        WriteEndpoint::AddSprintIssues,
        &base,
        &plan.into_wire_payload(),
    )?;
    ensure_empty_write_response(WriteEndpoint::AddSprintIssues, response)?;
    Ok(AppliedSprintMutation {
        operation: "sprint.add",
        applied: true,
        sprint_id: id,
        issue_keys: Some(keys),
    })
}
pub fn plan_sprint_close<T: JiraTransport>(
    client: &JiraClient<T>,
    id: u64,
    input: SprintCloseInput,
) -> Result<MutationPlan, AppError> {
    validate_sprint_close(id, &input)?;
    let path = encoded_path(&["rest", "agile", "1.0", "sprint", &id.to_string()])?;
    let sprint: JiraSprint = client.get_json_exact(&path, 200)?;
    if sprint.id != id || sprint.state != SprintState::Active {
        return Err(AppError::new(
            ErrorCode::InvalidState,
            "only an active sprint can be closed",
            RetrySafety::Safe,
        ));
    }
    let mut body = json!({"name":sprint.name,"state":"closed"});
    if let Some(v) = sprint.goal {
        body["goal"] = json!(v);
    }
    if let Some(v) = sprint.start_date {
        body["startDate"] = json!(v);
    }
    if let Some(v) = sprint.end_date {
        body["endDate"] = json!(v);
    }
    if let Some(v) = input.complete_date {
        body["completeDate"] = json!(v);
    }
    Ok(MutationPlan::dry_run(
        "sprint.close",
        json!({"sprint_id":id}),
        json!({"state":"closed"}),
        ValidationLevel::Passed,
        body,
    ))
}
pub fn apply_sprint_close<T: JiraTransport>(
    client: &JiraClient<T>,
    id: u64,
    plan: MutationPlan,
) -> Result<AppliedSprintMutation, AppError> {
    let path = encoded_path(&["rest", "agile", "1.0", "sprint", &id.to_string()])?;
    let response =
        client.jira_write(WriteEndpoint::CloseSprint, &path, &plan.into_wire_payload())?;
    let status = response.status;
    let sprint: JiraSprint = decode_write_response(WriteEndpoint::CloseSprint, response)?;
    if sprint.id != id || sprint.state != SprintState::Closed {
        return Err(invalid_write_success(WriteEndpoint::CloseSprint, status));
    }
    Ok(AppliedSprintMutation {
        operation: "sprint.close",
        applied: true,
        sprint_id: id,
        issue_keys: None,
    })
}
fn invalid() -> AppError {
    AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned invalid sprint data",
        RetrySafety::Safe,
    )
}
