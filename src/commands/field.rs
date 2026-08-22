use crate::client::{JiraClient, JiraTransport};
use crate::cursor::{PageState, QueryFingerprint, decode_cursor, encode_cursor};
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{FieldItem, JiraField, JiraPage, PageMeta};
use crate::output::{SuccessEnvelope, Warning};

pub fn field_list<T: JiraTransport>(
    client: &JiraClient<T>,
    query_text: Option<&str>,
    limit: u16,
    cursor: Option<&str>,
) -> Result<SuccessEnvelope<Vec<FieldItem>, PageMeta>, AppError> {
    let fingerprint = QueryFingerprint::new(&format!("query={query_text:?}&limit={limit}"));
    let offset = cursor
        .map(|cursor| decode_cursor(cursor, "field.list", &fingerprint))
        .transpose()?
        .map(offset_state)
        .transpose()?
        .unwrap_or(0);
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    if let Some(query_text) = query_text {
        query.append_pair("query", query_text);
    }
    query.append_pair("maxResults", &limit.to_string());
    if offset != 0 {
        query.append_pair("startAt", &offset.to_string());
    }
    let page: JiraPage<JiraField> =
        client.get_json(&format!("/rest/api/3/field/search?{}", query.finish()))?;
    page_envelope(page, offset, &fingerprint)
}

fn page_envelope(
    page: JiraPage<JiraField>,
    requested_offset: u64,
    fingerprint: &QueryFingerprint,
) -> Result<SuccessEnvelope<Vec<FieldItem>, PageMeta>, AppError> {
    if page.start_at != requested_offset {
        return Err(invalid_page());
    }
    let count = page.values.len();
    let next_offset = page
        .start_at
        .checked_add(count as u64)
        .ok_or_else(invalid_page)?;
    let has_more = page.is_last == Some(false) || next_offset < page.total;
    if has_more && count == 0 {
        return Err(invalid_page());
    }
    let next_cursor = has_more
        .then(|| encode_cursor("field.list", fingerprint, PageState::Offset(next_offset)))
        .transpose()?;
    let warnings = page
        .warning_messages
        .into_iter()
        .map(|message| Warning {
            code: "jira_warning".to_owned(),
            message,
        })
        .collect();
    let mut envelope = SuccessEnvelope::with_meta(
        page.values.into_iter().map(FieldItem::from).collect(),
        PageMeta { count, next_cursor },
    );
    envelope.warnings = warnings;
    Ok(envelope)
}

fn offset_state(state: PageState) -> Result<u64, AppError> {
    match state {
        PageState::Offset(offset) => Ok(offset),
        PageState::Token(_) => Err(AppError::new(
            ErrorCode::InvalidCursor,
            "the cursor has the wrong pagination state",
            RetrySafety::Safe,
        )),
    }
}

fn invalid_page() -> AppError {
    AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned invalid field pagination data",
        RetrySafety::Safe,
    )
}
