use std::fs;
use std::process::{Command as ProcessCommand, Stdio};

use clap::CommandFactory;
use jira_ops::cli::Cli;
use jira_ops::commands::local_docs::{CompletionShell, generate_completion, generate_man_pages};
use jira_ops::commands::settings::{canonical_issue_url, canonical_project_url};
use jira_ops::error::ErrorCode;
use tempfile::tempdir;
use url::Url;

#[test]
fn canonical_urls_encode_exactly_one_segment() {
    let site = Url::parse("https://acme.atlassian.net").unwrap();
    assert_eq!(
        canonical_issue_url(&site, "OPS-1").unwrap().as_str(),
        "https://acme.atlassian.net/browse/OPS-1"
    );
    assert_eq!(
        canonical_project_url(&site, "OPS / Core").unwrap().as_str(),
        "https://acme.atlassian.net/jira/software/projects/OPS%20%2F%20Core"
    );
}

#[test]
fn completion_is_generated_in_memory_for_each_supported_shell() {
    for shell in [
        CompletionShell::Bash,
        CompletionShell::Zsh,
        CompletionShell::Fish,
        CompletionShell::PowerShell,
        CompletionShell::Elvish,
    ] {
        let generated = generate_completion(shell).unwrap();
        assert!(generated.contains("jira-ops"), "{shell:?}");
        assert!(!generated.contains('\0'), "{shell:?}");
    }
}

#[test]
fn man_generation_writes_a_sorted_complete_set_to_an_empty_directory() {
    let parent = tempdir().unwrap();
    let output = parent.path().join("man");
    let files = generate_man_pages(&Cli::command(), &output).unwrap();
    assert!(!files.is_empty());
    assert!(files.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(files.iter().all(|path| path.components().count() == 1));
    for path in files {
        assert!(fs::metadata(output.join(path)).unwrap().len() > 0);
    }
}

#[test]
fn man_generation_refuses_unsafe_or_nonempty_targets() {
    let parent = tempdir().unwrap();
    let nonempty = parent.path().join("nonempty");
    fs::create_dir(&nonempty).unwrap();
    fs::write(nonempty.join("owned.txt"), "keep").unwrap();

    for path in [
        std::path::Path::new("/"),
        directories::BaseDirs::new().unwrap().home_dir(),
        nonempty.as_path(),
        std::path::Path::new("../escape"),
    ] {
        let error = generate_man_pages(&Cli::command(), path).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput, "{}", path.display());
    }

    #[cfg(unix)]
    {
        let target = parent.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let error = generate_man_pages(&Cli::command(), &link).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }
}

#[test]
fn local_cli_commands_work_without_a_token_or_keyring() {
    let home = tempdir().unwrap();
    let xdg = home.path().join("config");
    let config = include_str!("fixtures/config/saved.json");
    for path in [
        xdg.join("jira-ops/config.json"),
        home.path()
            .join("Library/Application Support/jira-ops/config.json"),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, config).unwrap();
    }

    let run = |args: &[&str], stdin: &str| {
        let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_jira-ops"))
            .env_clear()
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", &xdg)
            .env("JIRA_READ_ONLY", "1")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };

    let get = run(&["config", "get"], "");
    assert!(
        get.status.success(),
        "{}",
        String::from_utf8_lossy(&get.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&get.stdout).unwrap();
    assert_eq!(value["data"]["default_project"], serde_json::Value::Null);

    let set = run(
        &["config", "set", "--input", "-"],
        r#"{"default_project":"OPS","default_board":42}"#,
    );
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );

    let issue_url = run(&["url", "issue", "OPS-1"], "");
    assert!(issue_url.status.success());
    let value: serde_json::Value = serde_json::from_slice(&issue_url.stdout).unwrap();
    assert_eq!(
        value["data"]["url"],
        "https://example.atlassian.net/browse/OPS-1"
    );

    let unset = run(
        &["config", "unset", "--input", "-"],
        r#"{"default_project":true,"default_board":true}"#,
    );
    assert!(unset.status.success());

    let completion = run(&["completion", "bash"], "");
    assert!(completion.status.success());
    let value: serde_json::Value = serde_json::from_slice(&completion.stdout).unwrap();
    assert!(value["data"].as_str().unwrap().contains("jira-ops"));

    let output = home.path().join("generated-man");
    let man = run(&["man", "--output-dir", output.to_str().unwrap()], "");
    assert!(
        man.status.success(),
        "{}",
        String::from_utf8_lossy(&man.stderr)
    );
    assert!(output.join("jira-ops.1").is_file());
}
