use crate::client::{JiraClient, JiraTransport};
use crate::cursor::{PageState, QueryFingerprint, decode_cursor, encode_cursor};
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{JiraUserItem, PageMeta, UserItem};
use crate::output::SuccessEnvelope;

pub fn user_search<T: JiraTransport>(
    client: &JiraClient<T>,
    query: &str,
    limit: u16,
    cursor: Option<&str>,
) -> Result<SuccessEnvelope<Vec<UserItem>, PageMeta>, AppError> {
    if query.trim().is_empty() {
        return Err(invalid_input("user query must not be blank"));
    }
    let fingerprint = QueryFingerprint::new(&format!("query={query:?}&limit={limit}"));
    let offset = cursor
        .map(|cursor| decode_cursor(cursor, "user.search", &fingerprint))
        .transpose()?
        .map(offset_state)
        .transpose()?
        .unwrap_or(0);
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("query", query);
    serializer.append_pair("startAt", &offset.to_string());
    serializer.append_pair("maxResults", &limit.to_string());
    let items: Vec<JiraUserItem> =
        client.get_json(&format!("/rest/api/3/user/search?{}", serializer.finish()))?;
    if items.len() > usize::from(limit) {
        return Err(invalid_page());
    }
    let count = items.len();
    let next_offset = offset.checked_add(count as u64).ok_or_else(invalid_page)?;
    let next_cursor = (count == usize::from(limit))
        .then(|| encode_cursor("user.search", &fingerprint, PageState::Offset(next_offset)))
        .transpose()?;
    Ok(SuccessEnvelope::with_meta(
        items.into_iter().map(UserItem::from).collect(),
        PageMeta { count, next_cursor },
    ))
}

fn offset_state(state: PageState) -> Result<u64, AppError> {
    match state {
        PageState::Offset(value) => Ok(value),
        PageState::Token(_) => Err(invalid_page()),
    }
}

fn invalid_input(message: &str) -> AppError {
    AppError::new(ErrorCode::InvalidInput, message, RetrySafety::Safe)
}

fn invalid_page() -> AppError {
    AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned invalid user pagination data",
        RetrySafety::Safe,
    )
}
