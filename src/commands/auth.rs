use std::io::Read;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
use url::Url;
use uuid::Uuid;

use crate::auth::{SystemCredentialStore, login_commit, logout_commit, read_token_line};
use crate::client::{JiraClient, JiraTransport, unauthenticated_get_json};
use crate::config::{
    ConfigStore, CredentialSource, CredentialStore, EnvironmentSource, ResolvedCredential,
    SavedIdentity, environment_credential, environment_values, validate_site,
};
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::model::{Account, JiraAccount};
use crate::output::Warning;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TenantInfo {
    cloud_id: Uuid,
}

pub fn tenant_info(
    transport: &impl JiraTransport,
    site: &Url,
    timeout: Duration,
) -> Result<Uuid, AppError> {
    let url = site.join("_edge/tenant_info").map_err(|_| {
        AppError::new(
            ErrorCode::ConfigConflict,
            "the Jira tenant information URL is invalid",
            RetrySafety::Safe,
        )
    })?;
    if url.origin() != site.origin() {
        return Err(AppError::new(
            ErrorCode::ConfigConflict,
            "the Jira tenant information URL changed origin",
            RetrySafety::Safe,
        ));
    }
    unauthenticated_get_json::<_, TenantInfo>(transport, url, timeout).map(|info| info.cloud_id)
}

pub fn myself<T: JiraTransport>(client: &JiraClient<T>) -> Result<Account, AppError> {
    client
        .get_json::<JiraAccount>("/rest/api/3/myself")
        .map(Account::from)
}

#[derive(Debug, Serialize)]
pub struct AuthLoginData {
    pub site: Url,
    pub cloud_id: Uuid,
    pub email: String,
    pub account_id: String,
    pub display_name: String,
    pub credential_source: &'static str,
}

#[derive(Debug, Serialize)]
pub struct AuthLogoutData {
    pub removed_config: bool,
    pub removed_keyring: bool,
    pub environment_credentials_active: bool,
}

#[derive(Debug)]
pub struct CommandResult<T> {
    pub data: T,
    pub warnings: Vec<Warning>,
}

#[allow(clippy::too_many_arguments)]
pub fn auth_login(
    environment: &impl EnvironmentSource,
    config: &impl ConfigStore,
    credentials: &impl CredentialStore,
    transport: &impl JiraTransport,
    site: &str,
    email: &str,
    token_reader: &mut dyn Read,
    timeout: Duration,
) -> Result<CommandResult<AuthLoginData>, AppError> {
    if environment_values(environment).iter().any(Option::is_some) {
        return Err(AppError::new(
            ErrorCode::ConfigConflict,
            "auth login cannot run while Jira environment credentials are present",
            RetrySafety::Safe,
        ));
    }
    let site = validate_site(site).map_err(|_| {
        AppError::new(
            ErrorCode::InvalidInput,
            "--site must be an HTTPS atlassian.net origin",
            RetrySafety::Safe,
        )
    })?;
    if email.is_empty() || email.contains(['\0', '\r', '\n']) {
        return Err(AppError::new(
            ErrorCode::InvalidInput,
            "--email must be one non-empty line",
            RetrySafety::Safe,
        ));
    }

    let cloud_id = tenant_info(transport, &site, timeout)?;
    let token = read_token_line(token_reader, 16 * 1024)?;
    let pending = ResolvedCredential {
        site: site.clone(),
        cloud_id,
        email: email.to_owned(),
        account_id: None,
        token: token.clone(),
        source: CredentialSource::Keyring,
    };
    let account = myself(&JiraClient::new(transport, pending, timeout))?;
    if account.account_id.is_empty() || account.display_name.is_empty() {
        return Err(AppError::new(
            ErrorCode::ResponseInvalid,
            "Jira returned an incomplete account identity",
            RetrySafety::Safe,
        ));
    }
    let identity = SavedIdentity {
        site: site.clone(),
        cloud_id,
        email: email.to_owned(),
        account_id: account.account_id.clone(),
        default_project: None,
        default_board: None,
    };
    let commit = login_commit(
        config,
        credentials,
        crate::auth::NewLogin { identity, token },
    )?;

    Ok(CommandResult {
        data: AuthLoginData {
            site,
            cloud_id,
            email: email.to_owned(),
            account_id: account.account_id,
            display_name: account.display_name,
            credential_source: "keyring",
        },
        warnings: commit.warnings,
    })
}

pub fn auth_logout(
    environment: &impl EnvironmentSource,
    config: &impl ConfigStore,
    credentials: &impl CredentialStore,
) -> Result<AuthLogoutData, AppError> {
    let environment_credentials_active =
        environment_credential(&environment_values(environment))?.is_some();
    let commit = logout_commit(config, credentials)?;
    Ok(AuthLogoutData {
        removed_config: commit.removed_config,
        removed_keyring: commit.removed_keyring,
        environment_credentials_active,
    })
}

pub fn me_command(
    environment: &impl EnvironmentSource,
    config: &impl ConfigStore,
    credentials: &impl CredentialStore,
    transport: &impl JiraTransport,
    timeout: Duration,
) -> Result<Account, AppError> {
    myself(&super::authenticated_client(
        environment,
        config,
        credentials,
        transport,
        timeout,
    )?)
}

pub fn production_credentials() -> SystemCredentialStore {
    SystemCredentialStore
}
