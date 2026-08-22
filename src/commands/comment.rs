use serde_json::json;

use crate::adf::{adf_to_text, ensure_projected_text_within_limit};
use crate::client::{
    JiraClient, JiraTransport, WriteEndpoint, decode_write_response, invalid_write_success,
};
use crate::commands::issue::{encoded_path, validate_issue, validate_mutation_issue_key};
use crate::content::{ContentFormat, compile_content};
use crate::cursor::{PageState, QueryFingerprint, decode_cursor, encode_cursor};
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{
    AppliedComment, AppliedCommentMutation, AppliedIssueKey, CommentInput, CommentItem,
    IssueAssignee, JiraCommentPage, JiraCommentResponse, MutationPlan, PageMeta, ValidationLevel,
};
use crate::output::{SuccessEnvelope, Warning};

pub fn validate_comment_input(issue: &str, input: &CommentInput) -> Result<(), AppError> {
    validate_mutation_issue_key(issue)?;
    if input.body.is_empty() {
        return Err(crate::commands::schema_violation(
            "comment body must not be empty",
        ));
    }
    if input.internal && input.body.format() == ContentFormat::Adf {
        return Err(crate::commands::schema_violation(
            "internal comments do not accept explicit ADF content",
        ));
    }
    Ok(())
}

pub fn plan_comment(issue: &str, input: CommentInput) -> Result<MutationPlan, AppError> {
    validate_comment_input(issue, &input)?;
    let body = serde_json::to_value(&input.body)
        .map_err(|_| crate::commands::schema_violation("comment body is invalid"))?;
    let mut changes = json!({"body": body});
    if input.internal {
        changes
            .as_object_mut()
            .expect("comment changes are an object")
            .insert("internal".to_owned(), json!(true));
    }
    let wire_payload = if input.internal {
        compile_content(&input.body)?;
        let source = input.body.source_text().ok_or_else(|| {
            crate::commands::schema_violation("internal comment body must be text or markdown")
        })?;
        json!({"body":source,"public":false})
    } else {
        json!({"body":compile_content(&input.body)?})
    };
    Ok(MutationPlan::dry_run(
        "issue.comment",
        json!({"issue": issue}),
        changes,
        ValidationLevel::NotApplicable,
        wire_payload,
    ))
}

pub fn apply_comment<T: JiraTransport>(
    client: &JiraClient<T>,
    issue_key: &str,
    plan: MutationPlan,
) -> Result<AppliedCommentMutation, AppError> {
    validate_mutation_issue_key(issue_key)?;
    let internal = plan
        .changes
        .get("internal")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let (endpoint, path) = if internal {
        let issue_path = encoded_path(&["rest", "api", "3", "issue", issue_key])?;
        let _: serde_json::Value =
            client.get_json_exact(&format!("{issue_path}?fields=id"), 200)?;
        (
            WriteEndpoint::AddInternalComment,
            encoded_path(&["rest", "servicedeskapi", "request", issue_key, "comment"])?,
        )
    } else {
        (
            WriteEndpoint::AddComment,
            encoded_path(&["rest", "api", "3", "issue", issue_key, "comment"])?,
        )
    };
    let response = client
        .jira_write(endpoint, &path, &plan.into_wire_payload())
        .map_err(|error| map_internal_capability(error, internal))?;
    let status = response.status;
    let comment: JiraCommentResponse = decode_write_response(endpoint, response)?;
    if comment.id.trim().is_empty() {
        return Err(invalid_write_success(endpoint, status));
    }
    Ok(AppliedCommentMutation {
        operation: "issue.comment",
        applied: true,
        issue: AppliedIssueKey {
            key: issue_key.to_owned(),
        },
        comment: AppliedComment { id: comment.id },
    })
}

fn map_internal_capability(mut error: AppError, internal: bool) -> AppError {
    if internal && error.code == ErrorCode::NotFound {
        error.code = ErrorCode::UnsupportedJiraCapability;
        error.message =
            "the Jira tenant does not expose Jira Service Management comments".to_owned();
    }
    error
}

pub fn issue_comments<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    limit: u16,
    cursor: Option<&str>,
) -> Result<SuccessEnvelope<Vec<CommentItem>, PageMeta>, AppError> {
    validate_issue(issue)?;
    let fingerprint = QueryFingerprint::new(&format!("issue={issue:?}&limit={limit}"));
    let offset = cursor
        .map(|value| decode_cursor(value, "issue.comments", &fingerprint))
        .transpose()?
        .map(offset_state)
        .transpose()?
        .unwrap_or(0);
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("maxResults", &limit.to_string());
    if offset != 0 {
        query.append_pair("startAt", &offset.to_string());
    }
    let path = encoded_path(&["rest", "api", "3", "issue", issue, "comment"])?;
    let page: JiraCommentPage = client.get_json(&format!("{path}?{}", query.finish()))?;
    if page.comments.len() > usize::from(limit) {
        return Err(invalid_page());
    }
    let count = page.comments.len();
    let next_offset = page
        .start_at
        .checked_add(count as u64)
        .ok_or_else(invalid_page)?;
    let has_more = count != 0 && next_offset < page.total;
    let next_cursor = has_more
        .then(|| {
            encode_cursor(
                "issue.comments",
                &fingerprint,
                PageState::Offset(next_offset),
            )
        })
        .transpose()?;
    let data = page
        .comments
        .into_iter()
        .map(|comment| {
            Ok(CommentItem {
                id: comment.id,
                author: IssueAssignee::from(comment.author),
                body: comment_body_to_text(&comment.body)?,
                created: comment.created,
                updated: comment.updated,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let warnings = page
        .warning_messages
        .into_iter()
        .map(|message| Warning {
            code: "jira_warning".to_owned(),
            message,
        })
        .collect();
    let mut envelope = SuccessEnvelope::with_meta(data, PageMeta { count, next_cursor });
    envelope.warnings = warnings;
    Ok(envelope)
}

fn comment_body_to_text(body: &serde_json::Value) -> Result<String, AppError> {
    match body {
        serde_json::Value::String(text) => {
            ensure_projected_text_within_limit(text)?;
            Ok(text.clone())
        }
        _ => adf_to_text(body),
    }
}

fn offset_state(state: PageState) -> Result<u64, AppError> {
    match state {
        PageState::Offset(value) => Ok(value),
        PageState::Token(_) => Err(invalid_cursor()),
    }
}
fn invalid_cursor() -> AppError {
    AppError::new(
        ErrorCode::InvalidCursor,
        "the cursor has the wrong pagination state",
        RetrySafety::Safe,
    )
}
fn invalid_page() -> AppError {
    AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned invalid comment pagination data",
        RetrySafety::Safe,
    )
}
