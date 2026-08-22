use crate::client::{JiraClient, JiraTransport};
use crate::commands::issue::encoded_path;
use crate::cursor::{PageState, QueryFingerprint, decode_cursor, encode_cursor};
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{JiraReleasePage, PageMeta, ReleaseItem};
use crate::output::SuccessEnvelope;

pub fn release_list<T: JiraTransport>(
    client: &JiraClient<T>,
    project: &str,
    status: Option<&str>,
    limit: u16,
    cursor: Option<&str>,
) -> Result<SuccessEnvelope<Vec<ReleaseItem>, PageMeta>, AppError> {
    if project.trim().is_empty() {
        return Err(invalid_input("release project must not be blank"));
    }
    if status.is_some_and(|value| !matches!(value, "released" | "unreleased" | "archived")) {
        return Err(invalid_input(
            "release status must be released, unreleased, or archived",
        ));
    }
    let fingerprint = QueryFingerprint::new(&format!(
        "project={project:?}&status={status:?}&limit={limit}"
    ));
    let offset = cursor
        .map(|cursor| decode_cursor(cursor, "release.list", &fingerprint))
        .transpose()?
        .map(|state| match state {
            PageState::Offset(value) => Ok(value),
            PageState::Token(_) => Err(invalid_page()),
        })
        .transpose()?
        .unwrap_or(0);
    let path = encoded_path(&["rest", "api", "3", "project", project, "version"])?;
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    if let Some(status) = status {
        query.append_pair("status", status);
    }
    query.append_pair("startAt", &offset.to_string());
    query.append_pair("maxResults", &limit.to_string());
    let page: JiraReleasePage = client.get_json(&format!("{path}?{}", query.finish()))?;
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
        .then(|| encode_cursor("release.list", &fingerprint, PageState::Offset(next)))
        .transpose()?;
    Ok(SuccessEnvelope::with_meta(
        page.values.into_iter().map(ReleaseItem::from).collect(),
        PageMeta { count, next_cursor },
    ))
}

fn invalid_input(message: &str) -> AppError {
    AppError::new(ErrorCode::InvalidInput, message, RetrySafety::Safe)
}

fn invalid_page() -> AppError {
    AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned invalid release pagination data",
        RetrySafety::Safe,
    )
}
