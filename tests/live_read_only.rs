use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;

use jira_ops::client::{JiraClient, UreqTransport};
use jira_ops::commands::auth::{auth_login, auth_logout, me_command};
use jira_ops::commands::board::board_list;
use jira_ops::commands::comment::issue_comments;
use jira_ops::commands::epic::epic_jql;
use jira_ops::commands::field::field_list;
use jira_ops::commands::issue::{issue_create_meta, issue_get, issue_search};
use jira_ops::commands::project::project_list;
use jira_ops::commands::release::release_list;
use jira_ops::commands::server::server_info;
use jira_ops::commands::sprint::sprint_list;
use jira_ops::commands::transition::issue_transitions;
use jira_ops::commands::user::user_search;
use jira_ops::config::{
    CredentialKey, CredentialSource, CredentialStore, EnvironmentSource, FileConfigStore,
    ResolvedCredential, StoreError,
};
use jira_ops::error::ErrorCode;
use jira_ops::model::CreateMetaItem;
use secrecy::{ExposeSecret, SecretString};
use tempfile::tempdir;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

const TIMEOUT: Duration = Duration::from_secs(30);

#[test]
#[ignore = "requires the dedicated Jira Cloud sandbox credentials and seed issues"]
fn scoped_sandbox_read_contract() {
    let site = env_required("JIRA_OPS_LIVE_SITE");
    let email = env_required("JIRA_OPS_LIVE_EMAIL");
    let token = SecretString::from(env_required("JIRA_OPS_LIVE_API_TOKEN"));
    let temporary = tempdir().expect("temporary config directory");
    let config = FileConfigStore::at(temporary.path().join("jira-ops/config.json"));
    let credentials = MemoryCredentials::default();
    let environment = EmptyEnvironment;
    let transport = UreqTransport;
    let token_line = Zeroizing::new(format!("{}\n", token.expose_secret()));

    let login = auth_login(
        &environment,
        &config,
        &credentials,
        &transport,
        &site,
        &email,
        &mut token_line.as_bytes(),
        TIMEOUT,
    )
    .expect("scoped auth login");
    assert!(!login.data.account_id.is_empty());
    assert_eq!(
        me_command(&environment, &config, &credentials, &transport, TIMEOUT)
            .expect("saved credential me")
            .account_id,
        login.data.account_id
    );

    let client = client_from("JIRA_OPS_LIVE");
    server_info(&client).expect("server discovery");
    user_search(&client, &email, 1, None).expect("user discovery");
    let projects = project_list(&client, 1, None).expect("first project page");
    let project_cursor = projects
        .meta
        .expect("project metadata")
        .next_cursor
        .expect("sandbox has at least two visible projects");
    project_list(&client, 1, Some(&project_cursor)).expect("second project page");

    let fields = field_list(&client, None, 1, None).expect("first field page");
    let field_cursor = fields
        .meta
        .expect("field metadata")
        .next_cursor
        .expect("sandbox has at least two fields");
    field_list(&client, None, 1, Some(&field_cursor)).expect("second field page");

    for project in ["ACCL", "KAN"] {
        let issue_types = issue_create_meta(&client, project, None, 20, None)
            .expect("create issue type metadata");
        let issue_type_id = issue_types
            .data
            .iter()
            .find_map(|item| match item {
                CreateMetaItem::IssueType(value) if !value.subtask => Some(value.id.as_str()),
                _ => None,
            })
            .expect("non-subtask issue type");
        let metadata = issue_create_meta(&client, project, Some(issue_type_id), 100, None)
            .expect("create field metadata");
        assert!(!metadata.data.is_empty());
    }

    let board_id = env_required("JIRA_OPS_LIVE_BOARD_ID")
        .parse::<u64>()
        .expect("sandbox board ID");
    assert!(
        !board_list(&client, Some("ACCL"), None, 20, None)
            .expect("board discovery")
            .data
            .is_empty()
    );
    sprint_list(&client, board_id, None, 20, None).expect("sprint discovery");
    release_list(&client, "ACCL", None, 20, None).expect("release discovery");
    issue_search(&client, &epic_jql("ACCL", None).unwrap(), None, 20, None)
        .expect("epic discovery");

    let accl_issue = env_required("JIRA_OPS_LIVE_ACCL_ISSUE");
    let kan_issue = env_required("JIRA_OPS_LIVE_KAN_ISSUE");
    let expected_adf = env_required("JIRA_OPS_LIVE_ADF_TEXT");
    let description = vec!["description".to_owned()];
    let projected = issue_get(&client, &accl_issue, Some(&description)).expect("ADF issue read");
    assert_eq!(projected.data.description, Some(Some(expected_adf)));
    issue_get(&client, &kan_issue, None).expect("team-managed issue read");

    let search = issue_search(
        &client,
        "project in (ACCL, KAN) ORDER BY key",
        None,
        1,
        None,
    )
    .expect("first enhanced-search page");
    let search_cursor = search
        .meta
        .expect("search metadata")
        .next_cursor
        .expect("sandbox has at least two seeded issues");
    issue_search(
        &client,
        "project in (ACCL, KAN) ORDER BY key",
        None,
        1,
        Some(&search_cursor),
    )
    .expect("second enhanced-search page");
    issue_comments(&client, &accl_issue, 20, None).expect("comment read");
    assert!(
        !issue_transitions(&client, &accl_issue)
            .expect("expanded transitions")
            .data
            .is_empty()
    );

    let logout = auth_logout(&environment, &config, &credentials).expect("isolated logout");
    assert!(logout.removed_config);
    assert!(logout.removed_keyring);
    assert!(!config.path().exists());
}

#[test]
#[ignore = "requires a sandbox credential whose unauthorized response is recorded externally"]
fn ambiguous_unauthorized_status_is_auth_invalid() {
    let client = client_from("JIRA_OPS_LIVE_MISSING_SCOPE");
    let error = project_list(&client, 1, None).expect_err("unauthorized response");
    assert_eq!(error.status, Some(401));
    assert_eq!(error.code, ErrorCode::AuthInvalid);
}

#[test]
#[ignore = "requires a sandbox credential denied access to the seeded private issue"]
fn missing_project_permission_is_concealed_as_not_found() {
    let client = client_from("JIRA_OPS_LIVE_DENIED");
    let issue = env_required("JIRA_OPS_LIVE_DENIED_ISSUE");
    assert_eq!(
        issue_get(&client, &issue, None)
            .expect_err("permission failure")
            .code,
        ErrorCode::NotFound
    );
}

fn client_from(prefix: &str) -> JiraClient<UreqTransport> {
    JiraClient::new(
        UreqTransport,
        ResolvedCredential {
            site: Url::parse(&env_required(&format!("{prefix}_SITE"))).expect("sandbox site URL"),
            cloud_id: Uuid::parse_str(&env_required(&format!("{prefix}_CLOUD_ID")))
                .expect("sandbox cloud ID"),
            email: env_required(&format!("{prefix}_EMAIL")),
            account_id: None,
            token: SecretString::from(env_required(&format!("{prefix}_API_TOKEN"))),
            source: CredentialSource::Environment,
        },
        TIMEOUT,
    )
}

fn env_required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("required live-test variable {name} is missing"))
}

struct EmptyEnvironment;

impl EnvironmentSource for EmptyEnvironment {
    fn value(&self, _key: &str) -> Option<OsString> {
        None
    }
}

#[derive(Default)]
struct MemoryCredentials(RefCell<BTreeMap<String, SecretString>>);

impl CredentialStore for MemoryCredentials {
    fn get(&self, key: &CredentialKey) -> Result<SecretString, StoreError> {
        self.0
            .borrow()
            .get(&key.account)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    fn set(&self, key: &CredentialKey, value: &SecretString) -> Result<(), StoreError> {
        self.0
            .borrow_mut()
            .insert(key.account.clone(), value.clone());
        Ok(())
    }

    fn delete(&self, key: &CredentialKey) -> Result<(), StoreError> {
        self.0
            .borrow_mut()
            .remove(&key.account)
            .map(|_| ())
            .ok_or(StoreError::NotFound)
    }
}
