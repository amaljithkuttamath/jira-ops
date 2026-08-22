use assert_cmd::Command;
use jira_ops::config::SavedIdentity;
use serde_json::Value;
use tempfile::TempDir;

const ENVIRONMENT: [(&str, &str); 4] = [
    ("JIRA_SITE", "https://example.atlassian.net"),
    ("JIRA_CLOUD_ID", "00000000-0000-0000-0000-000000000000"),
    ("JIRA_EMAIL", "agent@example.com"),
    ("JIRA_API_TOKEN", "secret-test-token"),
];

#[test]
fn old_saved_identity_decodes_with_empty_defaults() {
    let saved: SavedIdentity = serde_json::from_str(include_str!("fixtures/config/saved.json"))
        .expect("legacy saved identity fixture");
    assert_eq!(saved.default_project, None);
    assert_eq!(saved.default_board, None);
}

fn auth_status(mask: u8, home: &TempDir) -> std::process::Output {
    let mut command = Command::cargo_bin("jira-ops").expect("jira-ops binary");
    command
        .env_clear()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .args(["auth", "status"]);
    for (index, (key, value)) in ENVIRONMENT.iter().enumerate() {
        if mask & (1 << index) != 0 {
            command.env(key, value);
        }
    }
    command.output().expect("run auth status")
}

#[test]
fn every_partial_environment_subset_is_a_config_conflict() {
    for mask in 1_u8..15 {
        let home = TempDir::new().unwrap();
        let output = auth_status(mask, &home);
        assert_eq!(output.status.code(), Some(3), "mask {mask:04b}");
        assert_eq!(output.stdout, b"", "mask {mask:04b}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        let value: Value = serde_json::from_str(&stderr).expect("valid error JSON");
        assert_eq!(value["error"]["code"], "config_conflict", "mask {mask:04b}");
        assert_eq!(value["error"]["retry_safety"], "safe");
        assert!(!stderr.contains("secret-test-token"));
        assert!(home.path().read_dir().unwrap().next().is_none());
    }
}

#[test]
fn complete_environment_tuple_reports_environment_mode_without_local_io() {
    let home = TempDir::new().unwrap();
    let output = auth_status(15, &home);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stderr, b"");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: Value = serde_json::from_str(&stdout).expect("valid success JSON");
    assert_eq!(value["data"]["configured"], true);
    assert_eq!(value["data"]["identity_source"], "environment");
    assert_eq!(value["data"]["credential_source"], "environment");
    assert_eq!(value["data"]["site"], "https://example.atlassian.net/");
    assert_eq!(
        value["data"]["cloud_id"],
        "00000000-0000-0000-0000-000000000000"
    );
    assert_eq!(value["data"]["email"], "agent@example.com");
    assert!(!stdout.contains("secret-test-token"));
    assert!(home.path().read_dir().unwrap().next().is_none());
}

#[test]
fn absent_environment_and_saved_config_reports_unconfigured_without_creating_files() {
    let home = TempDir::new().unwrap();
    let output = auth_status(0, &home);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stderr, b"");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: Value = serde_json::from_str(&stdout).expect("valid success JSON");
    assert_eq!(value["data"]["configured"], false);
    assert!(value["data"]["identity_source"].is_null());
    assert_eq!(value["data"]["credential_source"], "none");
    assert!(value["data"].get("site").is_none());
    assert!(value["data"].get("cloud_id").is_none());
    assert!(value["data"].get("email").is_none());
    assert!(home.path().read_dir().unwrap().next().is_none());
}

#[test]
fn logout_without_saved_or_environment_credentials_is_a_noop_success() {
    let home = TempDir::new().unwrap();
    let mut command = isolated_command(&home);
    let output = command.args(["auth", "logout"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stderr, b"");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["removed_config"], false);
    assert_eq!(value["data"]["removed_keyring"], false);
    assert_eq!(value["data"]["environment_credentials_active"], false);
}

#[test]
fn logout_reports_complete_environment_credentials_without_changing_them() {
    let home = TempDir::new().unwrap();
    let mut command = isolated_command(&home);
    for (key, value) in ENVIRONMENT {
        command.env(key, value);
    }
    let output = command.args(["auth", "logout"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["environment_credentials_active"], true);
    assert_eq!(value["data"]["removed_config"], false);
    assert_eq!(value["data"]["removed_keyring"], false);
}

#[test]
fn login_environment_conflict_is_rejected_before_network() {
    let home = TempDir::new().unwrap();
    let mut command = isolated_command(&home);
    command
        .env("JIRA_SITE", "https://other.atlassian.net")
        .args([
            "auth",
            "login",
            "--site",
            "https://example.atlassian.net",
            "--email",
            "agent@example.com",
            "--token-stdin",
        ])
        .write_stdin("must-not-be-sent\n");
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(output.stdout, b"");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let value: Value = serde_json::from_str(&stderr).unwrap();
    assert_eq!(value["error"]["code"], "config_conflict");
    assert!(!stderr.contains("must-not-be-sent"));
}

fn isolated_command(home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("jira-ops").expect("jira-ops binary");
    command
        .env_clear()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"));
    command
}
