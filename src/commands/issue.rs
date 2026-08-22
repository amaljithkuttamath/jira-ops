use std::collections::BTreeMap;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;

use crate::adf::adf_to_text;
use crate::client::{
    JiraClient, JiraTransport, WriteEndpoint, decode_write_response, invalid_write_success,
};
use crate::commands::{mutation_not_applied, schema_violation};
use crate::content::{ContentInput, compile_content};
use crate::cursor::{PageState, QueryFingerprint, decode_cursor, encode_cursor};
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{
    AppliedCreateIssue, AppliedCreatedIssue, AppliedIssueKey, AppliedIssueMutation,
    CreateIssueInput, CreateIssueTypeItem, CreateMetaItem, CreateMetaPageMeta, FieldMetadata,
    FieldSchema, InputKind, IssueAssignee, IssueProjection, JiraCreateFieldsPage,
    JiraCreateIssueResponse, JiraCreateIssueTypesPage, JiraEditMeta, JiraFieldMetadata, JiraIssue,
    JiraIssueAssignee, JiraIssueSearchPage, JiraIssueStatus, MutationPlan, PageMeta,
    UpdateIssueInput, ValidationLevel,
};
use crate::output::{SuccessEnvelope, Warning};
use serde_json::json;

const DEFAULT_FIELDS: [&str; 4] = ["summary", "status", "assignee", "updated"];
const PLANNING_PAGE_SIZE: u16 = 100;
const MAX_PLANNING_FIELDS: u64 = 10_000;
const MAX_PLANNING_PAGES: usize = 100;

pub fn validate_create_input(input: &CreateIssueInput) -> Result<(), AppError> {
    if input.project_key.trim().is_empty() || input.issue_type_id.trim().is_empty() {
        return Err(schema_violation(
            "project_key and issue_type_id must not be blank",
        ));
    }
    if !input.fields.contains_key("summary") {
        return Err(schema_violation("create fields must include summary"));
    }
    if input.fields.contains_key("project") || input.fields.contains_key("issuetype") {
        return Err(schema_violation(
            "create fields must not include project or issuetype",
        ));
    }
    Ok(())
}

pub fn validate_update_input(issue: &str, input: &UpdateIssueInput) -> Result<(), AppError> {
    validate_mutation_issue_key(issue)?;
    if input.set.is_empty() {
        return Err(schema_violation(
            "update set must contain at least one field",
        ));
    }
    Ok(())
}

pub(crate) fn validate_mutation_issue_key(issue: &str) -> Result<(), AppError> {
    let valid = issue == issue.trim()
        && issue.split_once('-').is_some_and(|(project, number)| {
            !project.is_empty()
                && !number.is_empty()
                && !number.contains('-')
                && project
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_uppercase)
                && project
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
                && number
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| matches!(byte, b'1'..=b'9'))
                && number.bytes().all(|byte| byte.is_ascii_digit())
        });
    if valid {
        Ok(())
    } else {
        Err(schema_violation(
            "mutation issue target must be an uppercase Jira key such as ACCL-1",
        ))
    }
}

pub fn plan_create_issue<T: JiraTransport>(
    client: &JiraClient<T>,
    input: CreateIssueInput,
) -> Result<MutationPlan, AppError> {
    validate_create_input(&input)?;
    let metadata = fetch_all_create_fields(client, &input.project_key, &input.issue_type_id)?;
    let (mut wire_fields, validation) =
        compile_fields(&metadata, &input.fields, false, true, false)?;
    wire_fields.insert("project".to_owned(), json!({"key": input.project_key}));
    wire_fields.insert("issuetype".to_owned(), json!({"id": input.issue_type_id}));
    Ok(MutationPlan::dry_run(
        "issue.create",
        json!({
            "project_key": input.project_key,
            "issue_type_id": input.issue_type_id
        }),
        json!({"fields": input.fields}),
        validation,
        json!({"fields": wire_fields}),
    ))
}

pub fn apply_create_issue<T: JiraTransport>(
    client: &JiraClient<T>,
    plan: MutationPlan,
) -> Result<AppliedCreateIssue, AppError> {
    let response = client.jira_write(
        WriteEndpoint::CreateIssue,
        "/rest/api/3/issue",
        &plan.into_wire_payload(),
    )?;
    let status = response.status;
    let created: JiraCreateIssueResponse =
        decode_write_response(WriteEndpoint::CreateIssue, response)?;
    if created.id.trim().is_empty() || created.key.trim().is_empty() {
        return Err(invalid_write_success(WriteEndpoint::CreateIssue, status));
    }
    let mut browse = client
        .verified_site()
        .join("browse")
        .map_err(|_| invalid_write_success(WriteEndpoint::CreateIssue, status))?;
    browse
        .path_segments_mut()
        .map_err(|_| invalid_write_success(WriteEndpoint::CreateIssue, status))?
        .push(&created.key);
    Ok(AppliedCreateIssue {
        operation: "issue.create",
        applied: true,
        issue: AppliedCreatedIssue {
            id: created.id,
            key: created.key,
            url: browse.into(),
        },
    })
}

pub fn plan_update_issue<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    input: UpdateIssueInput,
) -> Result<MutationPlan, AppError> {
    validate_update_input(issue, &input)?;
    let path = encoded_path(&["rest", "api", "3", "issue", issue, "editmeta"])
        .map_err(mutation_not_applied)?;
    let response: JiraEditMeta = client.get_json(&path).map_err(mutation_not_applied)?;
    let metadata = normalize_metadata_map(response.fields)?;
    let (wire_fields, validation) = compile_fields(&metadata, &input.set, true, false, true)?;
    let mut changes = json!({"set": input.set});
    if let Some(notify_users) = input.notify_users {
        changes
            .as_object_mut()
            .expect("update changes are an object")
            .insert("notify_users".to_owned(), json!(notify_users));
    }
    Ok(MutationPlan::dry_run(
        "issue.update",
        json!({"issue": issue}),
        changes,
        validation,
        json!({"fields": wire_fields}),
    ))
}

pub fn apply_update_issue<T: JiraTransport>(
    client: &JiraClient<T>,
    issue_key: &str,
    plan: MutationPlan,
) -> Result<AppliedIssueMutation, AppError> {
    validate_mutation_issue_key(issue_key)?;
    let path = mutation_path_with_notify(
        encoded_path(&["rest", "api", "3", "issue", issue_key])?,
        plan.changes.get("notify_users").and_then(Value::as_bool),
    );
    client.jira_write(WriteEndpoint::UpdateIssue, &path, &plan.into_wire_payload())?;
    Ok(AppliedIssueMutation {
        operation: "issue.update",
        applied: true,
        issue: AppliedIssueKey {
            key: issue_key.to_owned(),
        },
    })
}

pub(crate) fn mutation_path_with_notify(path: String, notify_users: Option<bool>) -> String {
    notify_users.map_or(path.clone(), |notify_users| {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("notifyUsers", if notify_users { "true" } else { "false" });
        format!("{path}?{}", query.finish())
    })
}

fn fetch_all_create_fields<T: JiraTransport>(
    client: &JiraClient<T>,
    project: &str,
    issue_type: &str,
) -> Result<Vec<FieldMetadata>, AppError> {
    let base = encoded_path(&[
        "rest",
        "api",
        "3",
        "issue",
        "createmeta",
        project,
        "issuetypes",
        issue_type,
    ])
    .map_err(mutation_not_applied)?;
    let mut offset = 0_u64;
    let mut declared_total = None;
    let mut fields = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut pages = 0_usize;
    loop {
        if pages == MAX_PLANNING_PAGES {
            return Err(invalid_planning_metadata());
        }
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("maxResults", &PLANNING_PAGE_SIZE.to_string());
        if offset != 0 {
            query.append_pair("startAt", &offset.to_string());
        }
        let page: JiraCreateFieldsPage = client
            .get_json(&format!("{base}?{}", query.finish()))
            .map_err(mutation_not_applied)?;
        pages += 1;
        if page.start_at != offset
            || page.fields.len() > usize::from(PLANNING_PAGE_SIZE)
            || page.total > MAX_PLANNING_FIELDS
            || declared_total.is_some_and(|total| total != page.total)
        {
            return Err(invalid_planning_metadata());
        }
        if declared_total.is_none() {
            fields.reserve(page.total as usize);
        }
        declared_total = Some(page.total);
        let count = page.fields.len() as u64;
        let next = offset
            .checked_add(count)
            .ok_or_else(invalid_planning_metadata)?;
        if next > page.total || (next < page.total && count == 0) {
            return Err(invalid_planning_metadata());
        }
        for field in page.fields {
            let field = normalize_field_metadata(field, None).map_err(mutation_not_applied)?;
            if !seen.insert(field.id.clone()) || fields.len() == MAX_PLANNING_FIELDS as usize {
                return Err(invalid_planning_metadata());
            }
            fields.push(field);
        }
        if next == page.total {
            break;
        }
        offset = next;
    }
    Ok(fields)
}

pub(crate) fn normalize_metadata_map(
    fields: BTreeMap<String, JiraFieldMetadata>,
) -> Result<Vec<FieldMetadata>, AppError> {
    let metadata = fields
        .into_iter()
        .map(|(id, field)| normalize_field_metadata(field, Some(&id)).map_err(mutation_not_applied))
        .collect::<Result<Vec<_>, _>>()?;
    ensure_unique_metadata(&metadata)?;
    Ok(metadata)
}

fn ensure_unique_metadata(fields: &[FieldMetadata]) -> Result<(), AppError> {
    let mut seen = std::collections::BTreeSet::new();
    if fields.iter().all(|field| seen.insert(field.id.as_str())) {
        Ok(())
    } else {
        Err(invalid_planning_metadata())
    }
}

pub(crate) fn compile_fields(
    metadata: &[FieldMetadata],
    input: &BTreeMap<String, Value>,
    require_set: bool,
    require_all_required: bool,
    allow_clear: bool,
) -> Result<(BTreeMap<String, Value>, ValidationLevel), AppError> {
    let by_id: BTreeMap<&str, &FieldMetadata> = metadata
        .iter()
        .map(|field| (field.id.as_str(), field))
        .collect();
    if require_all_required {
        for field in metadata {
            if field.required
                && !matches!(field.id.as_str(), "project" | "issuetype")
                && !input.contains_key(&field.id)
            {
                return Err(schema_violation(format!(
                    "required field {} is missing",
                    field.id
                )));
            }
        }
    }

    let mut validation = ValidationLevel::Passed;
    let mut wire = BTreeMap::new();
    for (id, value) in input {
        let field = by_id
            .get(id.as_str())
            .ok_or_else(|| schema_violation(format!("field {id} is not on the current screen")))?;
        if require_set && !field.operations.iter().any(|operation| operation == "set") {
            return Err(schema_violation(format!(
                "field {id} does not advertise the set operation"
            )));
        }
        let (compiled, partial) = compile_field_value(field, value, allow_clear)?;
        if partial {
            validation = ValidationLevel::Partial;
        }
        wire.insert(id.clone(), compiled);
    }
    Ok((wire, validation))
}

fn compile_field_value(
    field: &FieldMetadata,
    value: &Value,
    allow_clear: bool,
) -> Result<(Value, bool), AppError> {
    if value.is_null() {
        if allow_clear
            && !field.required
            && !matches!(field.input_kind, InputKind::Array | InputKind::Passthrough)
        {
            return Ok((Value::Null, false));
        }
        return Err(schema_violation(format!(
            "field {} cannot be cleared with null",
            field.id
        )));
    }
    if value.as_array().is_some_and(Vec::is_empty) {
        if field.required {
            return Err(schema_violation(format!(
                "required field {} cannot be an empty array",
                field.id
            )));
        }
        if allow_clear && field.input_kind != InputKind::Array {
            return Err(schema_violation(format!(
                "field {} cannot be cleared with an empty array",
                field.id
            )));
        }
        if field.input_kind != InputKind::Array {
            return Err(schema_violation(format!(
                "field {} has the wrong JSON type",
                field.id
            )));
        }
    }

    if field.input_kind == InputKind::AdfText {
        let input: ContentInput = serde_json::from_value(value.clone()).map_err(|_| {
            schema_violation(format!(
                "field {} requires a string or tagged content value",
                field.id
            ))
        })?;
        return compile_content(&input).map(|value| (value, false));
    }

    let valid_type = match field.input_kind {
        InputKind::String => value.is_string(),
        InputKind::AdfText => unreachable!("ADF fields compile before generic type checks"),
        InputKind::Number => value.is_number(),
        InputKind::Boolean => value.is_boolean(),
        InputKind::Array => value.is_array(),
        InputKind::Object => value.is_object(),
        InputKind::Passthrough => true,
    };
    if !valid_type {
        return Err(schema_violation(format!(
            "field {} has the wrong JSON type",
            field.id
        )));
    }
    if field.input_kind == InputKind::Array {
        return compile_array_value(field, value.as_array().expect("array type checked"));
    }
    if let Some(allowed) = nonempty_allowed_values(field) {
        return compile_allowed_candidate(field, value, allowed).map(|value| (value, false));
    }
    if field.input_kind == InputKind::Object {
        if !field.supported_selector_members.is_empty() {
            return compile_object_selector(field, value).map(|value| (value, false));
        }
        return Ok((value.clone(), true));
    }
    Ok((value.clone(), field.input_kind == InputKind::Passthrough))
}

fn compile_object_selector(field: &FieldMetadata, value: &Value) -> Result<Value, AppError> {
    let object = value.as_object().expect("object type checked");
    if object.len() != 1 {
        return Err(schema_violation(format!(
            "field {} requires exactly one supported selector member",
            field.id
        )));
    }
    let (member, value) = object.iter().next().expect("one selector member");
    if !field
        .supported_selector_members
        .iter()
        .any(|supported| supported == member)
        || value.as_str().is_none_or(str::is_empty)
    {
        return Err(schema_violation(format!(
            "field {} contains an invalid selector",
            field.id
        )));
    }
    Ok(Value::Object(object.clone()))
}

fn compile_array_value(field: &FieldMetadata, values: &[Value]) -> Result<(Value, bool), AppError> {
    let item_kind = field.schema.items.as_deref();
    let allowed = nonempty_allowed_values(field);
    let mut compiled = Vec::with_capacity(values.len());
    let partial =
        allowed.is_none() && !matches!(item_kind, Some("string" | "number" | "boolean" | "object"));
    for value in values {
        let valid_type = match item_kind {
            Some("string") => value.is_string(),
            Some("number") => value.is_number(),
            Some("boolean") => value.is_boolean(),
            Some("object") => value.is_object(),
            Some(_) | None => true,
        };
        if !valid_type {
            return Err(schema_violation(format!(
                "field {} has an array item with the wrong JSON type",
                field.id
            )));
        }
        if let Some(allowed) = allowed {
            compiled.push(compile_allowed_candidate(field, value, allowed)?);
        } else {
            compiled.push(value.clone());
        }
    }
    Ok((Value::Array(compiled), partial))
}

fn nonempty_allowed_values(field: &FieldMetadata) -> Option<&[Value]> {
    field
        .allowed_values
        .as_deref()
        .filter(|allowed| !allowed.is_empty())
}

fn compile_allowed_candidate(
    field: &FieldMetadata,
    candidate: &Value,
    allowed: &[Value],
) -> Result<Value, AppError> {
    for allowed_value in allowed {
        if candidate == allowed_value {
            if !candidate.is_object() {
                return Ok(candidate.clone());
            }
            return canonical_allowed_value(candidate, allowed_value).ok_or_else(|| {
                schema_violation(format!(
                    "field {} contains an invalid allowed-value selector",
                    field.id
                ))
            });
        }
        if selector_matches(candidate, allowed_value) {
            return canonical_allowed_value(candidate, allowed_value).ok_or_else(|| {
                schema_violation(format!(
                    "field {} contains an invalid allowed-value selector",
                    field.id
                ))
            });
        }
    }
    Err(schema_violation(format!(
        "field {} contains a value outside allowed_values",
        field.id
    )))
}

fn selector_matches(candidate: &Value, allowed: &Value) -> bool {
    let (Some(candidate), Some(allowed)) = (candidate.as_object(), allowed.as_object()) else {
        return false;
    };
    if candidate.is_empty()
        || candidate.keys().any(|key| {
            !matches!(
                key.as_str(),
                "id" | "value" | "account_id" | "name" | "display_name"
            )
        })
    {
        return false;
    }
    let has_selector = ["id", "value", "account_id", "name"]
        .into_iter()
        .any(|key| candidate.contains_key(key));
    has_selector
        && candidate
            .iter()
            .all(|(key, value)| allowed.get(key) == Some(value))
}

fn canonical_allowed_value(candidate: &Value, allowed: &Value) -> Option<Value> {
    let candidate = candidate.as_object()?;
    let allowed = allowed.as_object()?;
    for (public_key, wire_key) in [
        ("id", "id"),
        ("value", "value"),
        ("account_id", "accountId"),
        ("name", "name"),
    ] {
        if let Some(value) = candidate
            .get(public_key)
            .or_else(|| allowed.get(public_key))
        {
            return Some(json!({wire_key:value}));
        }
    }
    None
}

fn invalid_planning_metadata() -> AppError {
    mutation_not_applied(AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned invalid mutation metadata",
        RetrySafety::Safe,
    ))
}

pub fn issue_create_meta<T: JiraTransport>(
    client: &JiraClient<T>,
    project: &str,
    issue_type: Option<&str>,
    limit: u16,
    cursor: Option<&str>,
) -> Result<SuccessEnvelope<Vec<CreateMetaItem>, CreateMetaPageMeta>, AppError> {
    if project.is_empty() || issue_type == Some("") {
        return Err(AppError::new(
            ErrorCode::InvalidInput,
            "project and issue type identifiers must not be empty",
            RetrySafety::Safe,
        ));
    }
    let canonical =
        serde_json::to_string(&(project, issue_type, limit)).map_err(|_| internal_error())?;
    let fingerprint = QueryFingerprint::new(&canonical);
    let offset = cursor
        .map(|cursor| decode_cursor(cursor, "issue.create-meta", &fingerprint))
        .transpose()?
        .map(offset_state)
        .transpose()?
        .unwrap_or(0);
    let mut path = encoded_path(&[
        "rest",
        "api",
        "3",
        "issue",
        "createmeta",
        project,
        "issuetypes",
    ])?;
    if let Some(issue_type) = issue_type {
        path.push('/');
        path.push_str(&encoded_segment(issue_type)?);
    }
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("maxResults", &limit.to_string());
    if offset != 0 {
        query.append_pair("startAt", &offset.to_string());
    }
    path.push('?');
    path.push_str(&query.finish());

    let (items, start_at, total, kind) = if issue_type.is_some() {
        let page: JiraCreateFieldsPage = client.get_json(&path)?;
        if page.fields.len() > usize::from(limit) {
            return Err(invalid_page());
        }
        let items = page
            .fields
            .into_iter()
            .map(|field| normalize_field_metadata(field, None))
            .map(|item| item.map(CreateMetaItem::Field))
            .collect::<Result<Vec<_>, _>>()?;
        (items, page.start_at, page.total, "fields")
    } else {
        let page: JiraCreateIssueTypesPage = client.get_json(&path)?;
        if page.issue_types.len() > usize::from(limit) {
            return Err(invalid_page());
        }
        let items = page
            .issue_types
            .into_iter()
            .map(|item| {
                CreateMetaItem::IssueType(CreateIssueTypeItem {
                    id: item.id,
                    name: item.name,
                    subtask: item.subtask,
                })
            })
            .collect();
        (items, page.start_at, page.total, "issue_types")
    };
    if start_at != offset {
        return Err(invalid_page());
    }
    let count = items.len();
    let next_offset = start_at
        .checked_add(count as u64)
        .ok_or_else(invalid_page)?;
    if next_offset < total && count == 0 {
        return Err(invalid_page());
    }
    let next_cursor = (next_offset < total)
        .then(|| {
            encode_cursor(
                "issue.create-meta",
                &fingerprint,
                PageState::Offset(next_offset),
            )
        })
        .transpose()?;
    Ok(SuccessEnvelope::with_meta(
        items,
        CreateMetaPageMeta {
            kind,
            project: project.to_owned(),
            issue_type_id: issue_type.map(ToOwned::to_owned),
            count,
            next_cursor,
        },
    ))
}

pub fn issue_get<T: JiraTransport>(
    client: &JiraClient<T>,
    issue: &str,
    fields: Option<&[String]>,
) -> Result<SuccessEnvelope<IssueProjection>, AppError> {
    let selected = selected_fields(fields);
    let path = issue_path(issue, &selected)?;
    let issue: JiraIssue = client.get_json(&path)?;
    Ok(SuccessEnvelope::new(project_issue(issue, &selected)?))
}

pub fn issue_search<T: JiraTransport>(
    client: &JiraClient<T>,
    jql: &str,
    fields: Option<&[String]>,
    limit: u16,
    cursor: Option<&str>,
) -> Result<SuccessEnvelope<Vec<IssueProjection>, PageMeta>, AppError> {
    if jql.is_empty() {
        return Err(AppError::new(
            ErrorCode::InvalidInput,
            "the JQL query must not be empty",
            RetrySafety::Safe,
        ));
    }
    let selected = selected_fields(fields);
    let identity = SearchIdentity {
        jql,
        fields: &selected,
        limit,
    };
    let canonical = serde_json::to_string(&identity).map_err(|_| internal_error())?;
    let fingerprint = QueryFingerprint::new(&canonical);
    let next_page_token = cursor
        .map(|cursor| decode_cursor(cursor, "issue.search", &fingerprint))
        .transpose()?
        .map(token_state)
        .transpose()?;
    let request = SearchRequest {
        jql,
        fields: &selected,
        max_results: limit,
        next_page_token: next_page_token.as_deref(),
    };
    let page: JiraIssueSearchPage = client.post_json_read("/rest/api/3/search/jql", &request)?;
    search_envelope(page, &selected, &fingerprint)
}

fn selected_fields(fields: Option<&[String]>) -> Vec<String> {
    fields.map_or_else(
        || {
            DEFAULT_FIELDS
                .iter()
                .map(|field| (*field).to_owned())
                .collect()
        },
        <[String]>::to_vec,
    )
}

fn issue_path(issue: &str, fields: &[String]) -> Result<String, AppError> {
    validate_issue(issue)?;
    let path = encoded_path(&["rest", "api", "3", "issue", issue])?;
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("fields", &fields.join(","));
    Ok(format!("{path}?{}", query.finish()))
}

pub(crate) fn validate_issue(issue: &str) -> Result<(), AppError> {
    if issue.trim().is_empty() {
        return Err(AppError::new(
            ErrorCode::InvalidInput,
            "the issue identifier must not be empty",
            RetrySafety::Safe,
        ));
    }
    Ok(())
}

pub(crate) fn encoded_path(segments: &[&str]) -> Result<String, AppError> {
    let mut url = Url::parse("https://jira-ops.invalid/").map_err(|_| internal_error())?;
    url.path_segments_mut()
        .map_err(|_| internal_error())?
        .extend(segments.iter().copied());
    Ok(url.path().to_owned())
}

fn encoded_segment(segment: &str) -> Result<String, AppError> {
    Ok(encoded_path(&[segment])?.trim_start_matches('/').to_owned())
}

pub(crate) fn normalize_field_metadata(
    field: JiraFieldMetadata,
    fallback_id: Option<&str>,
) -> Result<FieldMetadata, AppError> {
    let id = non_blank(field.field_id)
        .or_else(|| non_blank(field.key))
        .or_else(|| {
            fallback_id
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
        })
        .ok_or_else(invalid_issue)?;
    let schema = field.schema.unwrap_or(crate::model::JiraFieldSchema {
        value_type: None,
        items: None,
        custom: None,
        system: None,
    });
    let input_kind = match (
        schema.value_type.as_deref(),
        schema.custom.as_deref(),
        id.as_str(),
    ) {
        (_, Some("com.atlassian.jira.plugin.system.customfieldtypes:textarea"), _)
        | (_, _, "description" | "environment") => InputKind::AdfText,
        (Some("string"), _, _) => InputKind::String,
        (Some("number"), _, _) => InputKind::Number,
        (Some("boolean"), _, _) => InputKind::Boolean,
        (Some("array"), _, _) => InputKind::Array,
        (Some("object" | "issuelink"), _, _) => InputKind::Object,
        _ => InputKind::Passthrough,
    };
    let allowed_values_complete = field.allowed_values.is_some();
    let allowed_values = field
        .allowed_values
        .map(|values| values.into_iter().map(project_allowed_value).collect());
    let supported_selector_members = supported_selector_members(
        &input_kind,
        schema.value_type.as_deref(),
        allowed_values.as_deref(),
    );
    Ok(FieldMetadata {
        id,
        name: field.name,
        required: field.required,
        operations: field.operations,
        schema: FieldSchema {
            value_type: schema.value_type,
            items: schema.items,
            custom: schema.custom,
            system: schema.system,
        },
        input_kind,
        supported_selector_members,
        allowed_values,
        allowed_values_complete,
    })
}

fn supported_selector_members(
    input_kind: &InputKind,
    schema_type: Option<&str>,
    allowed_values: Option<&[Value]>,
) -> Vec<String> {
    if input_kind != &InputKind::Object {
        return Vec::new();
    }
    if let Some(allowed_values) = allowed_values.filter(|values| !values.is_empty()) {
        return ["id", "value", "account_id", "name"]
            .into_iter()
            .filter(|member| {
                allowed_values
                    .iter()
                    .any(|value| value.get(*member).is_some())
            })
            .map(ToOwned::to_owned)
            .collect();
    }
    match schema_type {
        Some("issuelink") => ["id", "key"].into_iter().map(ToOwned::to_owned).collect(),
        _ => Vec::new(),
    }
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn project_allowed_value(value: Value) -> Value {
    let Value::Object(object) = value else {
        return value;
    };
    let mut projected = serde_json::Map::new();
    for (source, target) in [
        ("id", "id"),
        ("value", "value"),
        ("name", "name"),
        ("accountId", "account_id"),
        ("displayName", "display_name"),
    ] {
        if let Some(value) = object.get(source) {
            projected.insert(target.to_owned(), value.clone());
        }
    }
    Value::Object(projected)
}

fn project_issue(mut issue: JiraIssue, selected: &[String]) -> Result<IssueProjection, AppError> {
    let includes = |field: &str| selected.iter().any(|selected| selected == field);
    let summary = includes("summary")
        .then(|| take_nullable::<String>(&mut issue.fields, "summary"))
        .transpose()?;
    let status = includes("status")
        .then(|| take_nullable::<JiraIssueStatus>(&mut issue.fields, "status"))
        .transpose()?
        .map(|status| status.map(|status| status.name));
    let assignee = includes("assignee")
        .then(|| take_nullable::<JiraIssueAssignee>(&mut issue.fields, "assignee"))
        .transpose()?
        .map(|assignee| assignee.map(IssueAssignee::from));
    let updated = includes("updated")
        .then(|| take_nullable::<String>(&mut issue.fields, "updated"))
        .transpose()?;
    let description = includes("description")
        .then(|| take_description(&mut issue.fields))
        .transpose()?;

    let mut custom = BTreeMap::new();
    for field in selected {
        if !matches!(
            field.as_str(),
            "summary" | "status" | "assignee" | "updated" | "description"
        ) {
            custom.insert(
                field.clone(),
                issue.fields.remove(field).unwrap_or(Value::Null),
            );
        }
    }

    Ok(IssueProjection {
        key: issue.key,
        summary,
        status,
        assignee,
        updated,
        description,
        fields: (!custom.is_empty()).then_some(custom),
    })
}

fn take_nullable<T: DeserializeOwned>(
    fields: &mut BTreeMap<String, Value>,
    name: &str,
) -> Result<Option<T>, AppError> {
    match fields.remove(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value)
            .map(Some)
            .map_err(|_| invalid_issue()),
    }
}

fn take_description(fields: &mut BTreeMap<String, Value>) -> Result<Option<String>, AppError> {
    match fields.remove("description") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => adf_to_text(&value).map(Some),
    }
}

fn search_envelope(
    page: JiraIssueSearchPage,
    selected: &[String],
    fingerprint: &QueryFingerprint,
) -> Result<SuccessEnvelope<Vec<IssueProjection>, PageMeta>, AppError> {
    if page.is_last == page.next_page_token.is_some() || page.next_page_token.as_deref() == Some("")
    {
        return Err(invalid_page());
    }
    let next_cursor = page
        .next_page_token
        .map(|token| encode_cursor("issue.search", fingerprint, PageState::Token(token)))
        .transpose()?;
    let data = page
        .issues
        .into_iter()
        .map(|issue| project_issue(issue, selected))
        .collect::<Result<Vec<_>, _>>()?;
    let count = data.len();
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

fn token_state(state: PageState) -> Result<String, AppError> {
    match state {
        PageState::Token(token) => Ok(token),
        PageState::Offset(_) => Err(AppError::new(
            ErrorCode::InvalidCursor,
            "the cursor has the wrong pagination state",
            RetrySafety::Safe,
        )),
    }
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

#[derive(Serialize)]
struct SearchIdentity<'a> {
    jql: &'a str,
    fields: &'a [String],
    limit: u16,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchRequest<'a> {
    jql: &'a str,
    fields: &'a [String],
    max_results: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_page_token: Option<&'a str>,
}

fn invalid_issue() -> AppError {
    AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned malformed issue fields",
        RetrySafety::Safe,
    )
}

fn invalid_page() -> AppError {
    AppError::new(
        ErrorCode::ResponseInvalid,
        "Jira returned invalid issue search pagination data",
        RetrySafety::Safe,
    )
}

fn internal_error() -> AppError {
    AppError::new(
        ErrorCode::Internal,
        "failed to construct the Jira issue request",
        RetrySafety::Safe,
    )
}
