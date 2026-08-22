use crate::client::{JiraClient, JiraTransport};
use crate::cursor::{PageState, QueryFingerprint, decode_cursor, encode_cursor};
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{BoardItem, JiraBoardPage, PageMeta};
use crate::output::SuccessEnvelope;

pub fn board_list<T: JiraTransport>(
    client: &JiraClient<T>,
    project: Option<&str>,
    board_type: Option<&str>,
    limit: u16,
    cursor: Option<&str>,
) -> Result<SuccessEnvelope<Vec<BoardItem>, PageMeta>, AppError> {
    if project.is_some_and(|value| value.trim().is_empty()) {
        return Err(invalid_input("board project filter must not be blank"));
    }
    if board_type.is_some_and(|value| !matches!(value, "scrum" | "kanban" | "simple")) {
        return Err(invalid_input("board type must be scrum, kanban, or simple"));
    }
    let fingerprint = QueryFingerprint::new(&format!(
        "project={project:?}&type={board_type:?}&limit={limit}"
    ));
    let offset = decode_offset(cursor, "board.list", &fingerprint)?;
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    if let Some(project) = project {
        query.append_pair("projectKeyOrId", project);
    }
    if let Some(board_type) = board_type {
        query.append_pair("type", board_type);
    }
    query.append_pair("startAt", &offset.to_string());
    query.append_pair("maxResults", &limit.to_string());
    let page: JiraBoardPage =
        client.get_json(&format!("/rest/agile/1.0/board?{}", query.finish()))?;
    page_result(page, offset, limit, &fingerprint)
}

fn page_result(
    page: JiraBoardPage,
    offset: u64,
    limit: u16,
    fingerprint: &QueryFingerprint,
) -> Result<SuccessEnvelope<Vec<BoardItem>, PageMeta>, AppError> {
    if page.start_at != offset
        || page.max_results > u64::from(limit)
        || page.values.len() > usize::from(limit)
    {
        return Err(invalid_page());
    }
    let count = page.values.len();
    let next = offset.checked_add(count as u64).ok_or_else(invalid_page)?;
    if (page.is_last && next < page.total) || (!page.is_last && (count == 0 || next >= page.total))
    {
        return Err(invalid_page());
    }
    let next_cursor = (!page.is_last)
        .then(|| encode_cursor("board.list", fingerprint, PageState::Offset(next)))
        .transpose()?;
    Ok(SuccessEnvelope::with_meta(
        page.values.into_iter().map(BoardItem::from).collect(),
        PageMeta { count, next_cursor },
    ))
}

fn decode_offset(
    cursor: Option<&str>,
    command: &str,
    fingerprint: &QueryFingerprint,
) -> Result<u64, AppError> {
    cursor
        .map(|cursor| decode_cursor(cursor, command, fingerprint))
        .transpose()?
        .map(|state| match state {
            PageState::Offset(value) => Ok(value),
            PageState::Token(_) => Err(invalid_page()),
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn invalid_input(message: &str) -> AppError {
    AppError::new(ErrorCode::InvalidInput, message, RetrySafety::Safe)
}

fn invalid_page() -> AppError {
    AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned invalid board pagination data",
        RetrySafety::Safe,
    )
}
