use std::io::Read;

#[cfg(not(jira_ops_hierarchy_test))]
use secrecy::ExposeSecret;
use secrecy::SecretString;
use serde::Serialize;
use serde_json::json;
use url::Url;
use uuid::Uuid;

use crate::config::{
    ConfigStore, CredentialKey, CredentialStore, EnvironmentSource, SavedIdentity, StoreError,
    config_store_error, credential_store_error, environment_credential, environment_values,
};
use crate::error::{AppError, ErrorCode, RetrySafety};
use crate::output::Warning;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
    Saved,
    Environment,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusCredentialSource {
    KeyringConfigured,
    Environment,
    None,
}

#[derive(Debug, Serialize)]
pub struct AuthStatusData {
    pub configured: bool,
    pub identity_source: Option<IdentitySource>,
    pub credential_source: StatusCredentialSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<Url>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

pub fn auth_status(
    environment: &impl EnvironmentSource,
    config: &impl ConfigStore,
) -> Result<AuthStatusData, AppError> {
    let values = environment_values(environment);
    if let Some(environment) = environment_credential(&values)? {
        return Ok(AuthStatusData {
            configured: true,
            identity_source: Some(IdentitySource::Environment),
            credential_source: StatusCredentialSource::Environment,
            site: Some(environment.site),
            cloud_id: Some(environment.cloud_id),
            email: Some(environment.email),
        });
    }

    match config.load().map_err(config_store_error)? {
        Some(identity) => Ok(AuthStatusData {
            configured: true,
            identity_source: Some(IdentitySource::Saved),
            credential_source: StatusCredentialSource::KeyringConfigured,
            site: Some(identity.site),
            cloud_id: Some(identity.cloud_id),
            email: Some(identity.email),
        }),
        None => Ok(AuthStatusData {
            configured: false,
            identity_source: None,
            credential_source: StatusCredentialSource::None,
            site: None,
            cloud_id: None,
            email: None,
        }),
    }
}

#[derive(Clone)]
pub struct NewLogin {
    pub identity: SavedIdentity,
    pub token: SecretString,
}

#[derive(Debug)]
pub struct LoginCommit {
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct LogoutCommit {
    pub removed_config: bool,
    pub removed_keyring: bool,
}

pub fn login_commit(
    config: &impl ConfigStore,
    credentials: &impl CredentialStore,
    login: NewLogin,
) -> Result<LoginCommit, AppError> {
    let previous_identity = config.load().map_err(config_store_error)?;
    let new_key = CredentialKey::for_identity(&login.identity);
    let replaced_secret = optional_secret(credentials, &new_key)?;

    credentials
        .set(&new_key, &login.token)
        .map_err(credential_store_error)?;

    if config.atomic_replace(&login.identity).is_err() {
        let rollback = match replaced_secret {
            Some(secret) => credentials.set(&new_key, &secret),
            None => credentials.delete(&new_key),
        };
        if let Err(error) = rollback
            && error != StoreError::NotFound
        {
            return Err(partial_state(
                "login failed and the replaced keyring entry could not be restored",
                json!({"remaining_config":"previous","remaining_keyring":"new_or_unknown"}),
            ));
        }
        return Err(AppError::new(
            ErrorCode::LocalStatePartial,
            "the local Jira configuration could not be committed",
            RetrySafety::Safe,
        ));
    }

    let mut warnings = Vec::new();
    if let Some(previous) = previous_identity {
        let previous_key = CredentialKey::for_identity(&previous);
        if previous_key != new_key
            && let Err(error) = credentials.delete(&previous_key)
            && error != StoreError::NotFound
        {
            warnings.push(Warning {
                code: "keyring_cleanup_failed".to_owned(),
                message: "the previous keyring entry could not be removed".to_owned(),
            });
        }
    }

    Ok(LoginCommit { warnings })
}

pub fn logout_commit(
    config: &impl ConfigStore,
    credentials: &impl CredentialStore,
) -> Result<LogoutCommit, AppError> {
    let Some(identity) = config.load().map_err(config_store_error)? else {
        return Ok(LogoutCommit {
            removed_config: false,
            removed_keyring: false,
        });
    };
    let key = CredentialKey::for_identity(&identity);
    let removed_keyring = match credentials.delete(&key) {
        Ok(()) => true,
        Err(StoreError::NotFound) => false,
        Err(error) => return Err(credential_store_error(error)),
    };

    if config.remove().is_err() {
        return Err(partial_state(
            "the keyring entry was removed but the local configuration remains",
            json!({"remaining_config":true,"remaining_keyring":false}),
        ));
    }

    Ok(LogoutCommit {
        removed_config: true,
        removed_keyring,
    })
}

pub fn read_token_line(reader: &mut dyn Read, maximum: usize) -> Result<SecretString, AppError> {
    let read_limit = maximum.checked_add(1).ok_or_else(invalid_token)?;
    let mut bytes = Vec::new();
    reader
        .take(read_limit as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid_token())?;
    if bytes.len() > maximum {
        return Err(invalid_token());
    }

    if bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    } else if bytes.ends_with(b"\n") {
        bytes.truncate(bytes.len() - 1);
    }
    let token = String::from_utf8(bytes).map_err(|_| invalid_token())?;
    if token.is_empty() || token.contains(['\0', '\r', '\n']) {
        return Err(invalid_token());
    }
    Ok(SecretString::from(token))
}

pub struct SystemCredentialStore;

impl CredentialStore for SystemCredentialStore {
    fn get(&self, key: &CredentialKey) -> Result<SecretString, StoreError> {
        #[cfg(jira_ops_hierarchy_test)]
        panic!("forbidden production credential access under hierarchy test cfg: {key:?}");
        #[cfg(not(jira_ops_hierarchy_test))]
        {
            let entry = keyring::Entry::new("jira-ops", &key.account).map_err(map_keyring_error)?;
            let secret = entry.get_secret().map_err(map_keyring_error)?;
            String::from_utf8(secret)
                .map(SecretString::from)
                .map_err(|_| StoreError::InvalidData)
        }
    }

    fn set(&self, key: &CredentialKey, _value: &SecretString) -> Result<(), StoreError> {
        #[cfg(jira_ops_hierarchy_test)]
        panic!("forbidden production credential write under hierarchy test cfg: {key:?}");
        #[cfg(not(jira_ops_hierarchy_test))]
        {
            let entry = keyring::Entry::new("jira-ops", &key.account).map_err(map_keyring_error)?;
            entry
                .set_secret(_value.expose_secret().as_bytes())
                .map_err(map_keyring_error)
        }
    }

    fn delete(&self, key: &CredentialKey) -> Result<(), StoreError> {
        #[cfg(jira_ops_hierarchy_test)]
        panic!("forbidden production credential delete under hierarchy test cfg: {key:?}");
        #[cfg(not(jira_ops_hierarchy_test))]
        {
            let entry = keyring::Entry::new("jira-ops", &key.account).map_err(map_keyring_error)?;
            entry.delete_credential().map_err(map_keyring_error)
        }
    }
}

fn optional_secret(
    credentials: &impl CredentialStore,
    key: &CredentialKey,
) -> Result<Option<SecretString>, AppError> {
    match credentials.get(key) {
        Ok(secret) => Ok(Some(secret)),
        Err(StoreError::NotFound) => Ok(None),
        Err(error) => Err(credential_store_error(error)),
    }
}

fn partial_state(message: &str, details: serde_json::Value) -> AppError {
    let mut error = AppError::new(ErrorCode::LocalStatePartial, message, RetrySafety::Safe);
    error.details = Some(details);
    error
}

fn invalid_token() -> AppError {
    AppError::new(
        ErrorCode::InvalidInput,
        "token stdin must contain exactly one non-empty line of at most 16 KiB",
        RetrySafety::Safe,
    )
}

#[cfg(not(jira_ops_hierarchy_test))]
fn map_keyring_error(error: keyring::Error) -> StoreError {
    if matches!(error, keyring::Error::NoEntry) {
        StoreError::NotFound
    } else {
        StoreError::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    use secrecy::{ExposeSecret, SecretString};
    use url::Url;
    use uuid::Uuid;

    use super::{NewLogin, login_commit, logout_commit, read_token_line};
    use crate::config::{ConfigStore, CredentialKey, CredentialStore, SavedIdentity, StoreError};
    use crate::error::ErrorCode;

    struct FaultConfig {
        identity: RefCell<Option<SavedIdentity>>,
        fail_replace: Cell<bool>,
        fail_remove: Cell<bool>,
    }

    impl FaultConfig {
        fn with(identity: SavedIdentity) -> Self {
            Self {
                identity: RefCell::new(Some(identity)),
                fail_replace: Cell::new(false),
                fail_remove: Cell::new(false),
            }
        }
    }

    impl ConfigStore for FaultConfig {
        fn load(&self) -> Result<Option<SavedIdentity>, StoreError> {
            Ok(self.identity.borrow().clone())
        }

        fn atomic_replace(&self, value: &SavedIdentity) -> Result<(), StoreError> {
            if self.fail_replace.get() {
                return Err(StoreError::Unavailable);
            }
            self.identity.replace(Some(value.clone()));
            Ok(())
        }

        fn remove(&self) -> Result<(), StoreError> {
            if self.fail_remove.get() {
                return Err(StoreError::Unavailable);
            }
            self.identity.replace(None);
            Ok(())
        }
    }

    struct FaultKeyring {
        entries: RefCell<BTreeMap<String, String>>,
        fail_set: Cell<bool>,
        fail_delete_account: RefCell<Option<String>>,
    }

    impl FaultKeyring {
        fn with(identity: &SavedIdentity, token: &str) -> Self {
            Self {
                entries: RefCell::new(BTreeMap::from([(
                    CredentialKey::for_identity(identity).account,
                    token.to_owned(),
                )])),
                fail_set: Cell::new(false),
                fail_delete_account: RefCell::new(None),
            }
        }

        fn token(&self, identity: &SavedIdentity) -> Option<String> {
            self.entries
                .borrow()
                .get(&CredentialKey::for_identity(identity).account)
                .cloned()
        }
    }

    impl CredentialStore for FaultKeyring {
        fn get(&self, key: &CredentialKey) -> Result<SecretString, StoreError> {
            self.entries
                .borrow()
                .get(&key.account)
                .cloned()
                .map(SecretString::from)
                .ok_or(StoreError::NotFound)
        }

        fn set(&self, key: &CredentialKey, value: &SecretString) -> Result<(), StoreError> {
            if self.fail_set.get() {
                return Err(StoreError::Unavailable);
            }
            self.entries
                .borrow_mut()
                .insert(key.account.clone(), value.expose_secret().to_owned());
            Ok(())
        }

        fn delete(&self, key: &CredentialKey) -> Result<(), StoreError> {
            if self.fail_delete_account.borrow().as_deref() == Some(key.account.as_str()) {
                return Err(StoreError::Unavailable);
            }
            self.entries.borrow_mut().remove(&key.account);
            Ok(())
        }
    }

    #[test]
    fn failed_config_commit_restores_replaced_secret() {
        let old = identity("old-account");
        let config = FaultConfig::with(old.clone());
        config.fail_replace.set(true);
        let keyring = FaultKeyring::with(&old, "old-secret");

        let error = login_commit(&config, &keyring, login(old.clone(), "new-secret")).unwrap_err();

        assert_eq!(error.retry_safety, crate::error::RetrySafety::Safe);
        assert_eq!(config.identity.borrow().as_ref(), Some(&old));
        assert_eq!(keyring.token(&old).as_deref(), Some("old-secret"));
    }

    #[test]
    fn failed_new_key_set_preserves_old_login() {
        let old = identity("old-account");
        let new = identity("new-account");
        let config = FaultConfig::with(old.clone());
        let keyring = FaultKeyring::with(&old, "old-secret");
        keyring.fail_set.set(true);

        let error = login_commit(&config, &keyring, login(new.clone(), "new-secret")).unwrap_err();

        assert_eq!(error.code, ErrorCode::KeyringUnavailable);
        assert_eq!(config.identity.borrow().as_ref(), Some(&old));
        assert_eq!(keyring.token(&old).as_deref(), Some("old-secret"));
        assert_eq!(keyring.token(&new), None);
    }

    #[test]
    fn failed_config_commit_deletes_new_key_and_preserves_old_login() {
        let old = identity("old-account");
        let new = identity("new-account");
        let config = FaultConfig::with(old.clone());
        config.fail_replace.set(true);
        let keyring = FaultKeyring::with(&old, "old-secret");

        login_commit(&config, &keyring, login(new.clone(), "new-secret")).unwrap_err();

        assert_eq!(config.identity.borrow().as_ref(), Some(&old));
        assert_eq!(keyring.token(&old).as_deref(), Some("old-secret"));
        assert_eq!(keyring.token(&new), None);
    }

    #[test]
    fn old_key_cleanup_failure_keeps_new_login_and_returns_warning() {
        let old = identity("old-account");
        let new = identity("new-account");
        let config = FaultConfig::with(old.clone());
        let keyring = FaultKeyring::with(&old, "old-secret");
        keyring
            .fail_delete_account
            .replace(Some(CredentialKey::for_identity(&old).account));

        let result = login_commit(&config, &keyring, login(new.clone(), "new-secret")).unwrap();

        assert_eq!(config.identity.borrow().as_ref(), Some(&new));
        assert_eq!(keyring.token(&new).as_deref(), Some("new-secret"));
        assert_eq!(keyring.token(&old).as_deref(), Some("old-secret"));
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].code, "keyring_cleanup_failed");
    }

    #[test]
    fn logout_key_deletion_failure_leaves_config() {
        let old = identity("old-account");
        let config = FaultConfig::with(old.clone());
        let keyring = FaultKeyring::with(&old, "old-secret");
        keyring
            .fail_delete_account
            .replace(Some(CredentialKey::for_identity(&old).account));

        let error = logout_commit(&config, &keyring).unwrap_err();

        assert_eq!(error.code, ErrorCode::KeyringUnavailable);
        assert_eq!(config.identity.borrow().as_ref(), Some(&old));
        assert_eq!(keyring.token(&old).as_deref(), Some("old-secret"));
    }

    #[test]
    fn logout_config_removal_failure_reports_partial_state() {
        let old = identity("old-account");
        let config = FaultConfig::with(old.clone());
        config.fail_remove.set(true);
        let keyring = FaultKeyring::with(&old, "old-secret");

        let error = logout_commit(&config, &keyring).unwrap_err();

        assert_eq!(error.code, ErrorCode::LocalStatePartial);
        assert_eq!(config.identity.borrow().as_ref(), Some(&old));
        assert_eq!(keyring.token(&old), None);
        assert_eq!(error.details.unwrap()["remaining_config"], true);
    }

    #[test]
    fn token_reader_accepts_one_line_and_strips_one_terminator() {
        for (input, expected) in [
            (b"token\n".as_slice(), "token"),
            (b"token\r\n", "token"),
            (b"token", "token"),
        ] {
            let token = read_token_line(&mut &*input, 16 * 1024).unwrap();
            assert_eq!(token.expose_secret(), expected);
        }
    }

    #[test]
    fn token_reader_rejects_empty_multiline_nul_and_oversized_input() {
        for input in [
            Vec::new(),
            b"\n".to_vec(),
            b"one\ntwo\n".to_vec(),
            b"one\0two".to_vec(),
            vec![b'x'; 17],
        ] {
            let error = read_token_line(&mut input.as_slice(), 16).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidInput);
        }
    }

    fn identity(account_id: &str) -> SavedIdentity {
        SavedIdentity {
            site: Url::parse("https://example.atlassian.net").unwrap(),
            cloud_id: Uuid::nil(),
            email: "agent@example.com".to_owned(),
            account_id: account_id.to_owned(),
            default_project: None,
            default_board: None,
        }
    }

    fn login(identity: SavedIdentity, token: &str) -> NewLogin {
        NewLogin {
            identity,
            token: SecretString::from(token),
        }
    }
}
