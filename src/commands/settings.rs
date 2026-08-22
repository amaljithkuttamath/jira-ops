use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::{ConfigStore, SavedIdentity, config_store_error, validate_site};
use crate::error::{AppError, ErrorCode, RetrySafety};

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigPatch {
    #[serde(default)]
    pub default_project: Option<String>,
    #[serde(default)]
    pub default_board: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigUnsetInput {
    #[serde(default)]
    pub default_project: bool,
    #[serde(default)]
    pub default_board: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigDefaults {
    pub default_project: Option<String>,
    pub default_board: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UrlOutput {
    pub url: Url,
}

pub fn config_get(store: &impl ConfigStore) -> Result<ConfigDefaults, AppError> {
    let identity = load_identity(store)?;
    Ok(defaults(&identity))
}

pub fn config_set(
    store: &impl ConfigStore,
    patch: ConfigPatch,
) -> Result<ConfigDefaults, AppError> {
    validate_patch(&patch)?;
    let mut identity = load_identity(store)?;
    if let Some(project) = patch.default_project {
        identity.default_project = Some(project);
    }
    if let Some(board) = patch.default_board {
        identity.default_board = Some(board);
    }
    store
        .atomic_replace(&identity)
        .map_err(config_store_error)?;
    Ok(defaults(&identity))
}

pub fn config_unset(
    store: &impl ConfigStore,
    input: ConfigUnsetInput,
) -> Result<ConfigDefaults, AppError> {
    if !input.default_project && !input.default_board {
        return Err(invalid_input(
            "config unset must select default_project or default_board",
        ));
    }
    let mut identity = load_identity(store)?;
    if input.default_project {
        identity.default_project = None;
    }
    if input.default_board {
        identity.default_board = None;
    }
    store
        .atomic_replace(&identity)
        .map_err(config_store_error)?;
    Ok(defaults(&identity))
}

pub fn canonical_issue_url(site: &Url, issue: &str) -> Result<Url, AppError> {
    canonical_browse_url(site, &["browse"], issue, "issue")
}

pub fn canonical_project_url(site: &Url, project: &str) -> Result<Url, AppError> {
    canonical_browse_url(site, &["jira", "software", "projects"], project, "project")
}

pub fn configured_site(store: &impl ConfigStore) -> Result<Url, AppError> {
    Ok(load_identity(store)?.site)
}

fn canonical_browse_url(
    site: &Url,
    prefix: &[&str],
    identifier: &str,
    kind: &str,
) -> Result<Url, AppError> {
    if identifier.trim().is_empty() || identifier.contains(['\0', '\r', '\n']) {
        return Err(invalid_input(format!(
            "the {kind} identifier must not be blank"
        )));
    }
    let mut url = validate_site(site.as_str())?;
    url.path_segments_mut()
        .map_err(|_| invalid_input("the Jira site cannot be used as a browse URL"))?
        .pop_if_empty()
        .extend(prefix.iter().copied())
        .push(identifier);
    Ok(url)
}

fn validate_patch(patch: &ConfigPatch) -> Result<(), AppError> {
    if patch.default_project.is_none() && patch.default_board.is_none() {
        return Err(invalid_input(
            "config set must provide default_project or default_board",
        ));
    }
    if patch.default_project.as_ref().is_some_and(|project| {
        project.trim() != project
            || project.is_empty()
            || project.len() > 255
            || project.contains(['\0', '\r', '\n'])
    }) {
        return Err(invalid_input("default_project is invalid"));
    }
    if patch.default_board == Some(0) {
        return Err(invalid_input("default_board must be greater than zero"));
    }
    Ok(())
}

fn load_identity(store: &impl ConfigStore) -> Result<SavedIdentity, AppError> {
    store.load().map_err(config_store_error)?.ok_or_else(|| {
        AppError::new(
            ErrorCode::ConfigMissing,
            "Jira credentials are not configured",
            RetrySafety::Safe,
        )
    })
}

fn defaults(identity: &SavedIdentity) -> ConfigDefaults {
    ConfigDefaults {
        default_project: identity.default_project.clone(),
        default_board: identity.default_board,
    }
}

fn invalid_input(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::InvalidInput, message, RetrySafety::Safe)
}
