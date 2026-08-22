use serde_json::{Map, Value};

use crate::client::{
    JiraClient, JiraTransport, WriteEndpoint, decode_write_response, invalid_write_success,
};
use crate::commands::issue::encoded_path;
use crate::commands::schema_violation;
use crate::cursor::{PageState, QueryFingerprint, decode_cursor, encode_cursor};
use crate::error::{AppError, ErrorCode, OperationOutcome, RetrySafety};
use crate::model::{
    AppliedProjectCreate, CreatedProject, JiraPage, JiraProject, JiraProjectDetail, PageMeta,
    ProjectCreateInput, ProjectCreatePlan, ProjectDetail, ProjectItem, ProjectTemplate,
};
use crate::output::{SuccessEnvelope, Warning};

pub const PROJECT_TEMPLATE_REGISTRY_VERSION: u8 = 1;
const PROJECT_CREATE_PATH: &str = "/rest/api/3/project";

const PROJECT_TEMPLATES: &[ProjectTemplate] = &[
    ProjectTemplate {
        name: "Company-managed Kanban",
        project_type_key: "software",
        project_template_key: "com.pyxis.greenhopper.jira:gh-simplified-kanban-classic",
    },
    ProjectTemplate {
        name: "Company-managed Scrum",
        project_type_key: "software",
        project_template_key: "com.pyxis.greenhopper.jira:gh-simplified-scrum-classic",
    },
    ProjectTemplate {
        name: "Team-managed Kanban",
        project_type_key: "software",
        project_template_key: "com.pyxis.greenhopper.jira:gh-simplified-agility-kanban",
    },
    ProjectTemplate {
        name: "Team-managed Scrum",
        project_type_key: "software",
        project_template_key: "com.pyxis.greenhopper.jira:gh-simplified-agility-scrum",
    },
];

pub fn project_list<T: JiraTransport>(
    client: &JiraClient<T>,
    limit: u16,
    cursor: Option<&str>,
) -> Result<SuccessEnvelope<Vec<ProjectItem>, PageMeta>, AppError> {
    let fingerprint = QueryFingerprint::new(&format!("limit={limit}"));
    let offset = cursor
        .map(|cursor| decode_cursor(cursor, "project.list", &fingerprint))
        .transpose()?
        .map(offset_state)
        .transpose()?
        .unwrap_or(0);
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("maxResults", &limit.to_string());
    if offset != 0 {
        query.append_pair("startAt", &offset.to_string());
    }
    let page: JiraPage<JiraProject> =
        client.get_json(&format!("/rest/api/3/project/search?{}", query.finish()))?;
    page_envelope(page, offset, "project.list", &fingerprint)
}

pub fn project_get<T: JiraTransport>(
    client: &JiraClient<T>,
    project: &str,
) -> Result<SuccessEnvelope<ProjectDetail>, AppError> {
    if project.trim().is_empty() {
        return Err(AppError::new(
            ErrorCode::InvalidInput,
            "the project identifier must not be empty",
            RetrySafety::Safe,
        ));
    }
    let path = encoded_path(&["rest", "api", "3", "project", project])?;
    let detail: JiraProjectDetail = client.get_json(&path)?;
    Ok(SuccessEnvelope::new(detail.into()))
}

pub fn project_templates(project_type: Option<&str>) -> SuccessEnvelope<Vec<ProjectTemplate>> {
    let _registry_version = PROJECT_TEMPLATE_REGISTRY_VERSION;
    SuccessEnvelope::new(
        PROJECT_TEMPLATES
            .iter()
            .filter(|template| {
                project_type.is_none_or(|project_type| template.project_type_key == project_type)
            })
            .cloned()
            .collect(),
    )
}

pub fn validate_project_create_input(input: &ProjectCreateInput) -> Result<(), AppError> {
    let key = input.key.as_bytes();
    let key_is_valid = (2..=10).contains(&key.len())
        && key.first().is_some_and(u8::is_ascii_uppercase)
        && key
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_');
    if !key_is_valid {
        return Err(schema_violation(
            "project key must match uppercase [A-Z][A-Z0-9_]{1,9}",
        ));
    }
    let name_length = input.name.chars().count();
    if input.name.trim() != input.name || !(1..=80).contains(&name_length) {
        return Err(schema_violation(
            "project name must contain 1 to 80 characters without surrounding whitespace",
        ));
    }
    if input.project_type_key.trim() != input.project_type_key || input.project_type_key.is_empty()
    {
        return Err(schema_violation("project type must not be blank"));
    }
    if input.project_template_key.trim() != input.project_template_key
        || input.project_template_key.is_empty()
    {
        return Err(schema_violation("project template key must not be blank"));
    }
    if !PROJECT_TEMPLATES.iter().any(|template| {
        template.project_type_key == input.project_type_key
            && template.project_template_key == input.project_template_key
    }) {
        return Err(schema_violation(
            "project template is not in the local registry for the requested project type",
        ));
    }
    if input.lead_account_id.trim() != input.lead_account_id || input.lead_account_id.is_empty() {
        return Err(schema_violation("lead account ID must not be blank"));
    }
    if !input
        .assignee_type
        .as_deref()
        .is_none_or(|value| matches!(value, "UNASSIGNED" | "PROJECT_LEAD"))
    {
        return Err(schema_violation(
            "assignee type must be UNASSIGNED or PROJECT_LEAD",
        ));
    }
    Ok(())
}

pub fn plan_project_create(input: ProjectCreateInput) -> Result<ProjectCreatePlan, AppError> {
    validate_project_create_input(&input)?;
    let mut body = Map::from_iter([
        ("key".to_owned(), Value::String(input.key)),
        ("name".to_owned(), Value::String(input.name)),
        (
            "projectTypeKey".to_owned(),
            Value::String(input.project_type_key),
        ),
        (
            "projectTemplateKey".to_owned(),
            Value::String(input.project_template_key),
        ),
        (
            "leadAccountId".to_owned(),
            Value::String(input.lead_account_id),
        ),
        (
            "assigneeType".to_owned(),
            Value::String(
                input
                    .assignee_type
                    .unwrap_or_else(|| "UNASSIGNED".to_owned()),
            ),
        ),
    ]);
    if let Some(description) = input.description {
        body.insert("description".to_owned(), Value::String(description));
    }
    Ok(ProjectCreatePlan {
        operation: "project.create",
        method: "POST",
        path: PROJECT_CREATE_PATH,
        body: Value::Object(body),
    })
}

pub fn apply_project_create<T: JiraTransport>(
    client: &JiraClient<T>,
    plan: ProjectCreatePlan,
) -> Result<AppliedProjectCreate, AppError> {
    let response = client.jira_write(
        WriteEndpoint::CreateProject,
        PROJECT_CREATE_PATH,
        &plan.body,
    )?;
    let status = response.status;
    let project: CreatedProject = decode_write_response(WriteEndpoint::CreateProject, response)?;
    if project.id.trim().is_empty() || project.key.trim().is_empty() {
        return Err(invalid_write_success(WriteEndpoint::CreateProject, status));
    }
    Ok(AppliedProjectCreate {
        operation: "project.create",
        outcome: OperationOutcome::Applied,
        project,
    })
}

fn page_envelope(
    page: JiraPage<JiraProject>,
    requested_offset: u64,
    command: &str,
    fingerprint: &QueryFingerprint,
) -> Result<SuccessEnvelope<Vec<ProjectItem>, PageMeta>, AppError> {
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
        .then(|| encode_cursor(command, fingerprint, PageState::Offset(next_offset)))
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
        page.values.into_iter().map(ProjectItem::from).collect(),
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
        "Jira returned invalid project pagination data",
        RetrySafety::Safe,
    )
}
