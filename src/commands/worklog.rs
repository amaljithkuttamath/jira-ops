use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::adf::adf_to_text;
use crate::client::{
    JiraClient, JiraTransport, WriteEndpoint, decode_write_response, ensure_empty_write_response,
    invalid_write_success,
};
use crate::commands::issue::{encoded_path, validate_mutation_issue_key};
use crate::commands::{schema_violation, validate_confirmation};
use crate::content::compile_content;
use crate::cursor::{PageState, QueryFingerprint, decode_cursor, encode_cursor};
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{
    Account, AppliedIssueKey, AppliedWorklogMutation, EstimateAdjustment, JiraWorklog,
    JiraWorklogPage, MutationPlan, PageMeta, ValidationLevel, WorklogDeleteInput, WorklogItem,
    WorklogWriteInput,
};
use crate::output::SuccessEnvelope;

pub fn validate_worklog_write(input: &WorklogWriteInput) -> Result<(), AppError> {
    validate_duration(&input.time_spent)?;
    validate_adjustment(&input.adjustment)?;
    if let Some(started) = &input.started {
        normalize_started(started)?;
    }
    if let Some(comment) = &input.comment {
        compile_content(comment)?;
    }
    Ok(())
}

pub fn normalize_started(value: &str) -> Result<String, AppError> {
    let parsed = OffsetDateTime::parse(value, &Rfc3339).map_err(|_| {
        schema_violation("started must be an RFC3339 timestamp with an explicit offset")
    })?;
    let offset_seconds = parsed.offset().whole_seconds();
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let offset_minutes = offset_seconds.unsigned_abs() / 60;
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}{sign}{:02}{:02}",
        parsed.year(),
        u8::from(parsed.month()),
        parsed.day(),
        parsed.hour(),
        parsed.minute(),
        parsed.second(),
        parsed.millisecond(),
        offset_minutes / 60,
        offset_minutes % 60
    ))
}

pub fn compile_adjustment_query(
    adjustment: &EstimateAdjustment,
    notify_users: bool,
) -> Result<String, AppError> {
    validate_adjustment(adjustment)?;
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    match adjustment {
        EstimateAdjustment::Auto => {
            query.append_pair("adjustEstimate", "auto");
        }
        EstimateAdjustment::Leave => {
            query.append_pair("adjustEstimate", "leave");
        }
        EstimateAdjustment::New { new_estimate } => {
            query.append_pair("adjustEstimate", "new");
            query.append_pair("newEstimate", new_estimate);
        }
        EstimateAdjustment::Manual { reduce_by } => {
            query.append_pair("adjustEstimate", "manual");
            query.append_pair("reduceBy", reduce_by);
        }
    }
    query.append_pair("notifyUsers", if notify_users { "true" } else { "false" });
    Ok(query.finish())
}

pub fn worklog_list<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    limit: u16,
    cursor: Option<&str>,
) -> Result<SuccessEnvelope<Vec<WorklogItem>, PageMeta>, AppError> {
    validate_mutation_issue_key(issue)?;
    let fingerprint = QueryFingerprint::new(&format!("issue={issue:?}&limit={limit}"));
    let offset = cursor
        .map(|v| decode_cursor(v, "issue.worklog.list", &fingerprint))
        .transpose()?
        .map(|s| match s {
            PageState::Offset(v) => Ok(v),
            _ => Err(invalid_page()),
        })
        .transpose()?
        .unwrap_or(0);
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("startAt", &offset.to_string());
    query.append_pair("maxResults", &limit.to_string());
    let base = encoded_path(&["rest", "api", "3", "issue", issue, "worklog"])?;
    let page: JiraWorklogPage =
        client.get_json_exact(&format!("{base}?{}", query.finish()), 200)?;
    if page.start_at != offset || page.worklogs.len() > usize::from(limit) {
        return Err(invalid_page());
    }
    let count = page.worklogs.len();
    let next_offset = offset.checked_add(count as u64).ok_or_else(invalid_page)?;
    let next_cursor = (count > 0 && next_offset < page.total)
        .then(|| {
            encode_cursor(
                "issue.worklog.list",
                &fingerprint,
                PageState::Offset(next_offset),
            )
        })
        .transpose()?;
    let data = page
        .worklogs
        .into_iter()
        .map(project)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SuccessEnvelope::with_meta(
        data,
        PageMeta { count, next_cursor },
    ))
}

pub fn plan_worklog_add(issue: &str, input: WorklogWriteInput) -> Result<MutationPlan, AppError> {
    plan_write("issue.worklog.add", issue, None, input)
}
pub fn plan_worklog_update(
    issue: &str,
    worklog_id: &str,
    input: WorklogWriteInput,
) -> Result<MutationPlan, AppError> {
    validate_id(worklog_id)?;
    plan_write("issue.worklog.update", issue, Some(worklog_id), input)
}

fn plan_write(
    operation: &'static str,
    issue: &str,
    worklog_id: Option<&str>,
    input: WorklogWriteInput,
) -> Result<MutationPlan, AppError> {
    validate_mutation_issue_key(issue)?;
    validate_worklog_write(&input)?;
    let mut body = json!({"timeSpent":input.time_spent});
    if let Some(started) = input.started {
        body["started"] = json!(normalize_started(&started)?);
    }
    if let Some(comment) = input.comment {
        body["comment"] = compile_content(&comment)?;
    }
    let target = worklog_id.map_or_else(
        || json!({"issue":issue}),
        |id| json!({"issue":issue,"worklog_id":id}),
    );
    Ok(MutationPlan::dry_run(
        operation,
        target,
        json!({"adjustment":input.adjustment,"notify_users":input.notify_users}),
        ValidationLevel::NotApplicable,
        body,
    ))
}

pub fn apply_worklog_add<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    adjustment: &EstimateAdjustment,
    notify: bool,
    plan: MutationPlan,
) -> Result<AppliedWorklogMutation, AppError> {
    apply_write(
        client,
        issue,
        plan,
        WriteApply {
            endpoint: WriteEndpoint::AddWorklog,
            operation: "issue.worklog.add",
            id: None,
            adjustment,
            notify,
        },
    )
}
pub fn apply_worklog_update<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    id: &str,
    adjustment: &EstimateAdjustment,
    notify: bool,
    plan: MutationPlan,
) -> Result<AppliedWorklogMutation, AppError> {
    validate_id(id)?;
    apply_write(
        client,
        issue,
        plan,
        WriteApply {
            endpoint: WriteEndpoint::UpdateWorklog,
            operation: "issue.worklog.update",
            id: Some(id),
            adjustment,
            notify,
        },
    )
}

fn apply_write<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    plan: MutationPlan,
    write: WriteApply<'_>,
) -> Result<AppliedWorklogMutation, AppError> {
    validate_mutation_issue_key(issue)?;
    let mut parts = vec!["rest", "api", "3", "issue", issue, "worklog"];
    if let Some(id) = write.id {
        parts.push(id);
    }
    let base = encoded_path(&parts)?;
    let path = format!(
        "{base}?{}",
        compile_adjustment_query(write.adjustment, write.notify)?
    );
    let response = client.jira_write(write.endpoint, &path, &plan.into_wire_payload())?;
    let status = response.status;
    let result: JiraWorklog = decode_write_response(write.endpoint, response)?;
    if validate_id(&result.id).is_err() || write.id.is_some_and(|v| v != result.id) {
        return Err(invalid_write_success(write.endpoint, status));
    }
    Ok(AppliedWorklogMutation {
        operation: write.operation,
        applied: true,
        issue: AppliedIssueKey { key: issue.into() },
        worklog_id: result.id,
    })
}

struct WriteApply<'a> {
    endpoint: WriteEndpoint,
    operation: &'static str,
    id: Option<&'a str>,
    adjustment: &'a EstimateAdjustment,
    notify: bool,
}

pub fn plan_worklog_delete(
    issue: &str,
    id: &str,
    input: WorklogDeleteInput,
) -> Result<MutationPlan, AppError> {
    validate_mutation_issue_key(issue)?;
    validate_id(id)?;
    validate_confirmation(&input.confirm_worklog_id, id, "confirm_worklog_id")?;
    validate_adjustment(&input.adjustment)?;
    Ok(MutationPlan::dry_run(
        "issue.worklog.delete",
        json!({"issue":issue,"worklog_id":id}),
        json!({"adjustment":input.adjustment,"notify_users":input.notify_users}),
        ValidationLevel::NotApplicable,
        json!(null),
    ))
}
pub fn apply_worklog_delete<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    id: &str,
    adjustment: &EstimateAdjustment,
    notify: bool,
    _plan: MutationPlan,
) -> Result<AppliedWorklogMutation, AppError> {
    validate_mutation_issue_key(issue)?;
    validate_id(id)?;
    let base = encoded_path(&["rest", "api", "3", "issue", issue, "worklog", id])?;
    let response = client.jira_write_empty(
        WriteEndpoint::RemoveWorklog,
        &format!("{base}?{}", compile_adjustment_query(adjustment, notify)?),
    )?;
    ensure_empty_write_response(WriteEndpoint::RemoveWorklog, response)?;
    Ok(AppliedWorklogMutation {
        operation: "issue.worklog.delete",
        applied: true,
        issue: AppliedIssueKey { key: issue.into() },
        worklog_id: id.into(),
    })
}

fn project(value: JiraWorklog) -> Result<WorklogItem, AppError> {
    validate_id(&value.id).map_err(|_| invalid_page())?;
    let comment = value.comment.as_ref().map(adf_to_text).transpose()?;
    Ok(WorklogItem {
        id: value.id,
        author: Account::from(value.author),
        started: value.started,
        time_spent: value.time_spent,
        time_spent_seconds: value.time_spent_seconds,
        comment,
        updated: value.updated,
    })
}
fn validate_adjustment(value: &EstimateAdjustment) -> Result<(), AppError> {
    match value {
        EstimateAdjustment::New { new_estimate } => validate_duration(new_estimate),
        EstimateAdjustment::Manual { reduce_by } => validate_duration(reduce_by),
        _ => Ok(()),
    }
}
fn validate_duration(value: &str) -> Result<(), AppError> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value.split(' ').all(|token| {
            let (n, u) = token.split_at(token.len().saturating_sub(1));
            !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
                && n != "0"
                && matches!(u, "w" | "d" | "h" | "m")
        });
    if valid {
        Ok(())
    } else {
        Err(schema_violation(
            "duration must use Jira tokens such as 1w 2d 3h 30m",
        ))
    }
}
fn validate_id(value: &str) -> Result<(), AppError> {
    if value
        .as_bytes()
        .first()
        .is_some_and(|b| matches!(b, b'1'..=b'9'))
        && value.bytes().all(|b| b.is_ascii_digit())
        && value.len() <= 64
    {
        Ok(())
    } else {
        Err(schema_violation(
            "worklog ID must be a positive decimal identifier",
        ))
    }
}
fn invalid_page() -> AppError {
    AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned invalid worklog data",
        RetrySafety::Safe,
    )
}
