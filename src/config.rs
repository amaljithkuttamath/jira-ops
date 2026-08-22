use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[cfg(not(jira_ops_hierarchy_test))]
use directories::ProjectDirs;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use url::Url;
use uuid::Uuid;

use crate::error::{AppError, ErrorCode, RetrySafety};

pub const ENV_KEYS: [&str; 4] = ["JIRA_SITE", "JIRA_CLOUD_ID", "JIRA_EMAIL", "JIRA_API_TOKEN"];
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedIdentity {
    pub site: Url,
    pub cloud_id: Uuid,
    pub email: String,
    pub account_id: String,
    #[serde(default)]
    pub default_project: Option<String>,
    #[serde(default)]
    pub default_board: Option<u64>,
}

#[derive(Clone)]
pub struct EnvironmentCredential {
    pub site: Url,
    pub cloud_id: Uuid,
    pub email: String,
    pub token: SecretString,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    Environment,
    Keyring,
}

#[derive(Clone)]
pub struct ResolvedCredential {
    pub site: Url,
    pub cloud_id: Uuid,
    pub email: String,
    pub account_id: Option<String>,
    pub token: SecretString,
    pub source: CredentialSource,
}

pub struct PreparedCredential(PreparedCredentialSource);

enum PreparedCredentialSource {
    Environment(EnvironmentCredential),
    Saved(SavedIdentity),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialKey {
    pub account: String,
}

impl CredentialKey {
    pub fn for_identity(identity: &SavedIdentity) -> Self {
        Self {
            account: format!("{}:{}", identity.cloud_id, identity.account_id),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreError {
    NotFound,
    Unavailable,
    InvalidData,
}

pub trait EnvironmentSource {
    fn value(&self, key: &str) -> Option<OsString>;
}

pub struct ProcessEnvironment;

impl EnvironmentSource for ProcessEnvironment {
    fn value(&self, key: &str) -> Option<OsString> {
        #[cfg(jira_ops_hierarchy_test)]
        panic!("forbidden production environment access under hierarchy test cfg: {key}");
        #[cfg(not(jira_ops_hierarchy_test))]
        {
            std::env::var_os(key)
        }
    }
}

pub trait ConfigStore {
    fn load(&self) -> Result<Option<SavedIdentity>, StoreError>;
    fn atomic_replace(&self, value: &SavedIdentity) -> Result<(), StoreError>;
    fn remove(&self) -> Result<(), StoreError>;
}

pub trait CredentialStore {
    fn get(&self, key: &CredentialKey) -> Result<SecretString, StoreError>;
    fn set(&self, key: &CredentialKey, value: &SecretString) -> Result<(), StoreError>;
    fn delete(&self, key: &CredentialKey) -> Result<(), StoreError>;
}

pub struct FileConfigStore {
    path: PathBuf,
}

impl FileConfigStore {
    pub fn for_current_user() -> Result<Self, StoreError> {
        #[cfg(jira_ops_hierarchy_test)]
        panic!("forbidden production config access under hierarchy test cfg");
        #[cfg(not(jira_ops_hierarchy_test))]
        {
            let directories =
                ProjectDirs::from("", "", "jira-ops").ok_or(StoreError::Unavailable)?;
            Ok(Self {
                path: directories.config_dir().join("config.json"),
            })
        }
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ConfigStore for FileConfigStore {
    fn load(&self) -> Result<Option<SavedIdentity>, StoreError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(StoreError::Unavailable),
        };
        if file.metadata().map_err(|_| StoreError::Unavailable)?.len() > MAX_CONFIG_BYTES {
            return Err(StoreError::InvalidData);
        }
        serde_json::from_reader(file)
            .map(Some)
            .map_err(|_| StoreError::InvalidData)
    }

    fn atomic_replace(&self, value: &SavedIdentity) -> Result<(), StoreError> {
        let parent = self.path.parent().ok_or(StoreError::Unavailable)?;
        fs::create_dir_all(parent).map_err(|_| StoreError::Unavailable)?;
        set_directory_permissions(parent)?;

        let mut temporary = NamedTempFile::new_in(parent).map_err(|_| StoreError::Unavailable)?;
        set_file_permissions(temporary.path())?;
        serde_json::to_writer(&mut temporary, value).map_err(|_| StoreError::InvalidData)?;
        temporary
            .write_all(b"\n")
            .map_err(|_| StoreError::Unavailable)?;
        temporary.flush().map_err(|_| StoreError::Unavailable)?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| StoreError::Unavailable)?;
        temporary
            .persist(&self.path)
            .map_err(|_| StoreError::Unavailable)?;
        let _ = sync_directory(parent);
        Ok(())
    }

    fn remove(&self) -> Result<(), StoreError> {
        match fs::remove_file(&self.path) {
            Ok(()) => {
                if let Some(parent) = self.path.parent() {
                    let _ = sync_directory(parent);
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(StoreError::Unavailable),
        }
    }
}

pub fn environment_values(
    environment: &impl EnvironmentSource,
) -> [Option<OsString>; ENV_KEYS.len()] {
    ENV_KEYS.map(|key| environment.value(key))
}

pub fn environment_credential(
    values: &[Option<OsString>; ENV_KEYS.len()],
) -> Result<Option<EnvironmentCredential>, AppError> {
    let present = values.iter().filter(|value| value.is_some()).count();
    if present == 0 {
        return Ok(None);
    }
    if present != ENV_KEYS.len() {
        return Err(config_conflict(
            "Jira environment credentials must provide all four required variables",
        ));
    }

    let strings: Vec<String> = values
        .iter()
        .map(|value| {
            value
                .as_ref()
                .expect("all environment values are present")
                .clone()
                .into_string()
                .map_err(|_| config_conflict("Jira environment credentials must be UTF-8"))
        })
        .collect::<Result<_, _>>()?;
    let site = validate_site(&strings[0])?;
    let cloud_id = Uuid::parse_str(&strings[1])
        .map_err(|_| config_conflict("JIRA_CLOUD_ID must be a UUID"))?;
    let email = validate_identity_text("JIRA_EMAIL", &strings[2])?;
    let token = validate_token_value(&strings[3])?;

    Ok(Some(EnvironmentCredential {
        site,
        cloud_id,
        email,
        token,
    }))
}

pub fn resolve_credential(
    environment: &impl EnvironmentSource,
    config: &impl ConfigStore,
    credentials: &impl CredentialStore,
) -> Result<ResolvedCredential, AppError> {
    let prepared = prepare_credential(environment, config)?;
    resolve_prepared_credential(prepared, credentials)
}

pub fn prepare_credential(
    environment: &impl EnvironmentSource,
    config: &impl ConfigStore,
) -> Result<PreparedCredential, AppError> {
    let values = environment_values(environment);
    if let Some(environment) = environment_credential(&values)? {
        return Ok(PreparedCredential(PreparedCredentialSource::Environment(
            environment,
        )));
    }

    let identity = config.load().map_err(config_store_error)?.ok_or_else(|| {
        AppError::new(
            ErrorCode::ConfigMissing,
            "Jira credentials are not configured",
            RetrySafety::Safe,
        )
    })?;
    let site = validate_site(identity.site.as_str()).map_err(|_| invalid_saved_identity())?;
    validate_identity_text("saved Jira email", &identity.email)
        .map_err(|_| invalid_saved_identity())?;
    validate_identity_text("saved Jira account ID", &identity.account_id)
        .map_err(|_| invalid_saved_identity())?;
    Ok(PreparedCredential(PreparedCredentialSource::Saved(
        SavedIdentity { site, ..identity },
    )))
}

pub fn resolve_prepared_credential(
    prepared: PreparedCredential,
    credentials: &impl CredentialStore,
) -> Result<ResolvedCredential, AppError> {
    match prepared.0 {
        PreparedCredentialSource::Environment(environment) => Ok(ResolvedCredential {
            site: environment.site,
            cloud_id: environment.cloud_id,
            email: environment.email,
            account_id: None,
            token: environment.token,
            source: CredentialSource::Environment,
        }),
        PreparedCredentialSource::Saved(identity) => {
            let key = CredentialKey::for_identity(&identity);
            let token = credentials.get(&key).map_err(credential_store_error)?;
            Ok(ResolvedCredential {
                site: identity.site,
                cloud_id: identity.cloud_id,
                email: identity.email,
                account_id: Some(identity.account_id),
                token,
                source: CredentialSource::Keyring,
            })
        }
    }
}

fn invalid_saved_identity() -> AppError {
    AppError::new(
        ErrorCode::LocalStatePartial,
        "local Jira configuration is unavailable or invalid",
        RetrySafety::Safe,
    )
}

pub fn validate_site(value: &str) -> Result<Url, AppError> {
    let site = Url::parse(value).map_err(|_| config_conflict("Jira site must be a valid URL"))?;
    let host = site
        .host_str()
        .ok_or_else(|| config_conflict("Jira site must include a hostname"))?;
    let tenant = host.strip_suffix(".atlassian.net").unwrap_or_default();
    if site.scheme() != "https"
        || tenant.is_empty()
        || !site.username().is_empty()
        || site.password().is_some()
        || site.port().is_some()
        || site.path() != "/"
        || site.query().is_some()
        || site.fragment().is_some()
    {
        return Err(config_conflict(
            "Jira site must be an HTTPS atlassian.net origin",
        ));
    }
    Ok(site)
}

fn validate_identity_text(name: &str, value: &str) -> Result<String, AppError> {
    if value.is_empty() || value.contains(['\0', '\r', '\n']) {
        return Err(config_conflict(format!("{name} is invalid")));
    }
    Ok(value.to_owned())
}

fn validate_token_value(value: &str) -> Result<SecretString, AppError> {
    if value.is_empty() || value.len() > 16 * 1024 || value.contains(['\0', '\r', '\n']) {
        return Err(config_conflict("JIRA_API_TOKEN is invalid"));
    }
    Ok(SecretString::from(value))
}

pub fn config_store_error(error: StoreError) -> AppError {
    match error {
        StoreError::NotFound => AppError::new(
            ErrorCode::ConfigMissing,
            "Jira credentials are not configured",
            RetrySafety::Safe,
        ),
        StoreError::Unavailable | StoreError::InvalidData => AppError::new(
            ErrorCode::LocalStatePartial,
            "local Jira configuration is unavailable or invalid",
            RetrySafety::Safe,
        ),
    }
}

pub fn credential_store_error(error: StoreError) -> AppError {
    match error {
        StoreError::NotFound => AppError::new(
            ErrorCode::AuthMissing,
            "the saved Jira credential is missing",
            RetrySafety::Safe,
        ),
        StoreError::Unavailable | StoreError::InvalidData => AppError::new(
            ErrorCode::KeyringUnavailable,
            "the system credential store is unavailable",
            RetrySafety::Safe,
        ),
    }
}

pub fn config_conflict(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::ConfigConflict, message, RetrySafety::Safe)
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| StoreError::Unavailable)
}

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(unix)]
fn set_file_permissions(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| StoreError::Unavailable)
}

#[cfg(not(unix))]
fn set_file_permissions(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), StoreError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| StoreError::Unavailable)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::fs;

    use secrecy::{ExposeSecret, SecretString};
    use tempfile::TempDir;
    use url::Url;
    use uuid::Uuid;

    use super::{
        ConfigStore, CredentialKey, CredentialSource, CredentialStore, EnvironmentSource,
        FileConfigStore, SavedIdentity, StoreError, resolve_credential,
    };

    struct MapEnvironment(BTreeMap<String, OsString>);

    impl EnvironmentSource for MapEnvironment {
        fn value(&self, key: &str) -> Option<OsString> {
            self.0.get(key).cloned()
        }
    }

    struct CountingConfig {
        loads: Cell<usize>,
    }

    impl ConfigStore for CountingConfig {
        fn load(&self) -> Result<Option<SavedIdentity>, StoreError> {
            self.loads.set(self.loads.get() + 1);
            Ok(Some(identity("saved")))
        }

        fn atomic_replace(&self, _value: &SavedIdentity) -> Result<(), StoreError> {
            unreachable!()
        }

        fn remove(&self) -> Result<(), StoreError> {
            unreachable!()
        }
    }

    struct CountingCredentials {
        gets: Cell<usize>,
    }

    impl CredentialStore for CountingCredentials {
        fn get(&self, _key: &CredentialKey) -> Result<SecretString, StoreError> {
            self.gets.set(self.gets.get() + 1);
            Ok(SecretString::from("saved-token"))
        }

        fn set(&self, _key: &CredentialKey, _value: &SecretString) -> Result<(), StoreError> {
            unreachable!()
        }

        fn delete(&self, _key: &CredentialKey) -> Result<(), StoreError> {
            unreachable!()
        }
    }

    #[test]
    fn complete_environment_mode_never_reads_config_or_keyring() {
        let environment = MapEnvironment(BTreeMap::from([
            (
                "JIRA_SITE".to_owned(),
                "https://example.atlassian.net".into(),
            ),
            ("JIRA_CLOUD_ID".to_owned(), Uuid::nil().to_string().into()),
            ("JIRA_EMAIL".to_owned(), "agent@example.com".into()),
            ("JIRA_API_TOKEN".to_owned(), "environment-token".into()),
        ]));
        let config = CountingConfig {
            loads: Cell::new(0),
        };
        let credentials = CountingCredentials { gets: Cell::new(0) };

        let resolved = resolve_credential(&environment, &config, &credentials).unwrap();

        assert_eq!(resolved.source, CredentialSource::Environment);
        assert_eq!(resolved.token.expose_secret(), "environment-token");
        assert_eq!(config.loads.get(), 0);
        assert_eq!(credentials.gets.get(), 0);
    }

    #[test]
    fn file_config_store_atomically_round_trips_and_removes_identity() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("jira-ops").join("config.json");
        let store = FileConfigStore::at(&path);
        let first = identity("first");
        let second = identity("second");

        assert_eq!(store.load().unwrap(), None);
        store.atomic_replace(&first).unwrap();
        assert_eq!(store.load().unwrap(), Some(first));
        store.atomic_replace(&second).unwrap();
        assert_eq!(store.load().unwrap(), Some(second));
        assert_eq!(fs::read_to_string(&path).unwrap().matches('\n').count(), 1);
        assert_eq!(path.parent().unwrap().read_dir().unwrap().count(), 1);
        store.remove().unwrap();
        assert_eq!(store.load().unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn file_config_store_uses_user_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TempDir::new().unwrap();
        let path = directory.path().join("jira-ops").join("config.json");
        let store = FileConfigStore::at(&path);
        store.atomic_replace(&identity("account")).unwrap();

        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn saved_fixture_decodes_to_the_stable_identity_shape() {
        let parsed: SavedIdentity =
            serde_json::from_str(include_str!("../tests/fixtures/config/saved.json")).unwrap();
        assert_eq!(parsed, identity("abc123"));
    }

    fn identity(account_id: &str) -> SavedIdentity {
        SavedIdentity {
            site: Url::parse("https://example.atlassian.net/").unwrap(),
            cloud_id: Uuid::nil(),
            email: "agent@example.com".to_owned(),
            account_id: account_id.to_owned(),
            default_project: None,
            default_board: None,
        }
    }
}
