use std::ffi::OsString;

use assert_cmd::Command;
use clap::CommandFactory;
use jira_ops::cli::Cli;
use serde_json::Value;

const DOCUMENTS: [(&str, &str); 9] = [
    ("README.md", include_str!("../README.md")),
    ("docs/commands.md", include_str!("../docs/commands.md")),
    (
        "docs/agent-guide.md",
        include_str!("../docs/agent-guide.md"),
    ),
    ("docs/auth.md", include_str!("../docs/auth.md")),
    ("docs/recipes.md", include_str!("../docs/recipes.md")),
    ("docs/releasing.md", include_str!("../docs/releasing.md")),
    ("SECURITY.md", include_str!("../SECURITY.md")),
    ("CHANGELOG.md", include_str!("../CHANGELOG.md")),
    ("LICENSE-MIT", include_str!("../LICENSE-MIT")),
];

const JIRA_ENVIRONMENT_KEYS: [&str; 5] = [
    "JIRA_SITE",
    "JIRA_CLOUD_ID",
    "JIRA_EMAIL",
    "JIRA_API_TOKEN",
    "JIRA_READ_ONLY",
];

#[test]
fn every_declared_command_example_parses_without_credentials() {
    let schema = schema_all();
    let commands = schema["data"]["commands"]
        .as_array()
        .expect("schema command array");

    for command in commands {
        let name = command["command"].as_str().expect("command name");
        let argv = command["example"]["argv"]
            .as_array()
            .expect("example argv")
            .iter()
            .map(|argument| OsString::from(argument.as_str().expect("string argv member")))
            .collect::<Vec<_>>();

        Cli::parse_args(argv)
            .unwrap_or_else(|error| panic!("schema example for {name} no longer parses: {error}"));
    }
}

#[test]
fn clap_help_renders_for_the_root_and_every_command_node() {
    assert_help_tree(&Cli::command(), "jira-ops");
}

#[test]
fn every_scoped_schema_is_available_offline() {
    let schema = schema_all();
    let commands = schema["data"]["commands"]
        .as_array()
        .expect("schema command array");

    for command in commands {
        let name = command["command"].as_str().expect("command name");
        let mut process = isolated_binary();
        process.arg("schema").args(name.split('.'));
        let output = process.output().expect("run scoped schema");
        assert!(
            output.status.success(),
            "offline schema failed for {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty(), "schema wrote stderr for {name}");
        let value: Value = serde_json::from_slice(&output.stdout).expect("scoped schema JSON");
        assert_eq!(value["data"]["command"], name);
    }
}

#[test]
fn every_current_command_is_discoverable_from_public_docs() {
    let schema = schema_all();
    let public_docs = [
        include_str!("../README.md"),
        include_str!("../docs/commands.md"),
        include_str!("../docs/agent-guide.md"),
        include_str!("../docs/auth.md"),
        include_str!("../docs/recipes.md"),
    ]
    .join("\n");

    for command in schema["data"]["commands"]
        .as_array()
        .expect("schema command array")
    {
        let dotted = command["command"].as_str().expect("command name");
        let shell = dotted.replace('.', " ");
        assert!(
            public_docs.contains(&format!("`{shell}`"))
                || public_docs.contains(&format!("jira-ops {shell}")),
            "{dotted} is not reachable from the public documentation"
        );
    }
}

#[test]
fn homebrew_install_and_upgrade_are_copy_paste_ready() {
    let readme = include_str!("../README.md");
    let install = "brew install amaljithkuttamath/tap/jira-ops";
    let upgrade = "brew upgrade jira-ops";

    assert!(readme.contains(install), "README is missing {install}");
    assert!(readme.contains(upgrade), "README is missing {upgrade}");

    let install_position = readme.find(install).expect("Homebrew install position");
    let release_position = readme
        .find("[GitHub Releases]")
        .expect("GitHub Releases install position");
    assert!(
        install_position < release_position,
        "Homebrew should be the first actionable install path"
    );
}

#[test]
fn documentation_code_blocks_contain_no_literal_credentials() {
    for (path, document) in DOCUMENTS {
        for (line_number, line) in fenced_code_lines(document) {
            let compact = line.trim();
            let lower = compact.to_ascii_lowercase();
            assert!(
                !compact.contains("ATATT")
                    && !lower.contains("authorization: basic ")
                    && !lower.contains("authorization: bearer eyj")
                    && !compact.contains(concat!("-----BEGIN ", "PRIVATE KEY-----")),
                "possible literal credential in {path}:{line_number}"
            );

            if let Some((_, value)) = compact.split_once("JIRA_API_TOKEN=") {
                let value = value.trim().trim_matches(['\'', '"']);
                assert!(
                    value.is_empty() || value.starts_with('$'),
                    "literal JIRA_API_TOKEN assignment in {path}:{line_number}"
                );
            }
        }
    }
}

#[test]
fn documented_mutation_examples_are_dry_runs() {
    const MUTATIONS: [&str; 22] = [
        "jira-ops project create",
        "jira-ops issue create",
        "jira-ops issue clone",
        "jira-ops issue delete",
        "jira-ops issue update",
        "jira-ops issue comment",
        "jira-ops issue transition",
        "jira-ops issue assign",
        "jira-ops issue link add",
        "jira-ops issue link remove",
        "jira-ops issue remote-link add",
        "jira-ops issue remote-link remove",
        "jira-ops issue worklog add",
        "jira-ops issue worklog update",
        "jira-ops issue worklog delete",
        "jira-ops epic create",
        "jira-ops epic add",
        "jira-ops epic remove",
        "jira-ops sprint add",
        "jira-ops sprint close",
        "jira-ops issue watcher add",
        "jira-ops issue watcher remove",
    ];

    for (path, document) in DOCUMENTS {
        for block in fenced_code_blocks(document) {
            if MUTATIONS.iter().any(|mutation| block.contains(mutation)) {
                assert!(
                    !block.contains("--apply"),
                    "mutation example is not a dry run in {path}"
                );
            }
        }
    }
}

fn schema_all() -> Value {
    let output = isolated_binary()
        .args(["schema", "--all"])
        .output()
        .expect("run schema --all");
    assert!(
        output.status.success(),
        "schema --all failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).expect("schema --all JSON")
}

fn isolated_binary() -> Command {
    let mut command = Command::cargo_bin("jira-ops").expect("jira-ops binary");
    for key in JIRA_ENVIRONMENT_KEYS {
        command.env_remove(key);
    }
    command
}

fn assert_help_tree(command: &clap::Command, path: &str) {
    let help = command.clone().render_long_help().to_string();
    assert!(help.contains("Usage:"), "missing Usage section for {path}");

    for child in command.get_subcommands() {
        assert_help_tree(child, &format!("{path} {}", child.get_name()));
    }
}

fn fenced_code_lines(document: &str) -> Vec<(usize, &str)> {
    let mut lines = Vec::new();
    let mut in_block = false;
    for (index, line) in document.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            in_block = !in_block;
        } else if in_block {
            lines.push((index + 1, line));
        }
    }
    lines
}

fn fenced_code_blocks(document: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = None::<String>;
    for line in document.lines() {
        if line.trim_start().starts_with("```") {
            if let Some(block) = current.take() {
                blocks.push(block);
            } else {
                current = Some(String::new());
            }
        } else if let Some(block) = current.as_mut() {
            block.push_str(line);
            block.push('\n');
        }
    }
    blocks
}
