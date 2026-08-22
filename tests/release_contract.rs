#![cfg(unix)]

use sha2::{Digest, Sha256};
use std::fs;
use std::process::Command;

fn package_script() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("package-release.sh")
}

fn release_workflow() -> String {
    fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".github")
            .join("workflows")
            .join("release.yml"),
    )
    .expect("read release workflow")
}

fn ci_workflow() -> String {
    fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".github")
            .join("workflows")
            .join("ci.yml"),
    )
    .expect("read CI workflow")
}

fn job_block<'a>(workflow: &'a str, job: &str, next_job: Option<&str>) -> &'a str {
    let start = format!("  {job}:\n");
    let workflow = workflow
        .split_once(&start)
        .unwrap_or_else(|| panic!("missing {job} job"))
        .1;
    match next_job {
        Some(next_job) => {
            workflow
                .split_once(&format!("  {next_job}:\n"))
                .unwrap_or_else(|| panic!("missing {next_job} job"))
                .0
        }
        None => workflow,
    }
}

#[test]
fn release_build_and_publish_are_gated_by_quality_on_the_tag_commit() {
    let workflow = release_workflow();
    let quality = job_block(&workflow, "quality", Some("build"));
    let build = job_block(&workflow, "build", Some("publish"));
    let publish = job_block(&workflow, "publish", Some("archive-scan"));

    for expected in [
        "needs: validate",
        "ref: ${{ github.sha }}",
        "fetch-depth: 0",
        "cargo fmt --all -- --check",
        "cargo clippy --locked --all-targets --all-features -- -D warnings",
        "cargo test --locked --all-targets --all-features",
        "cargo build --locked --release",
        "command: check",
        "cargo llvm-cov --locked --all-features --workspace --fail-under-lines 80",
        "gitleaks/gitleaks-action@e0c47f4f8be36e29cdc102c57e68cb5cbf0e8d1e",
    ] {
        assert!(
            quality.contains(expected),
            "release gate is missing {expected:?}"
        );
    }
    for expected in [
        "needs: [validate, quality]",
        "contents: read",
        "id-token: write",
        "attestations: write",
    ] {
        assert!(
            build.contains(expected),
            "build gate is missing {expected:?}"
        );
    }
    assert!(
        publish.contains("needs: [quality, build, archive-scan]"),
        "publish must wait for quality, build, and archive scan"
    );
    assert!(
        publish.contains("--repo \"$GITHUB_REPOSITORY\""),
        "publish must identify the repository without relying on a checkout"
    );
}

#[test]
fn pull_request_secret_scan_provides_the_workflow_token() {
    let workflow = ci_workflow();
    let secrets = job_block(&workflow, "secrets", None);

    assert!(
        secrets.contains("GITHUB_TOKEN: ${{ github.token }}"),
        "Gitleaks requires the workflow token when scanning a pull request"
    );
}

#[test]
fn release_workflow_verifies_gitleaks_binary_and_attests_release_artifacts() {
    let workflow = release_workflow();
    let build = job_block(&workflow, "build", Some("publish"));
    let archive_scan = job_block(&workflow, "archive-scan", None);

    for expected in [
        "https://github.com/gitleaks/gitleaks/releases/download/v8.28.0/gitleaks_8.28.0_linux_x64.tar.gz",
        "a65b5253807a68ac0cafa4414031fd740aeb55f54fb7e55f386acb52e6a840eb",
        "sha256sum --check --status",
        "tar -xzf \"$gitleaks_archive\"",
        "\"$gitleaks_dir/gitleaks\" dir scan scan --no-banner --redact",
    ] {
        assert!(
            archive_scan.contains(expected),
            "release integrity control is missing {expected:?}"
        );
    }
    assert!(
        !archive_scan.contains("go install github.com/zricethezav/gitleaks"),
        "archive scan must not install Gitleaks with go install"
    );
    for expected in [
        "actions/attest-build-provenance@4d101475d8b20a2381f78447822ac1eab6504dd8",
        "subject-path: dist/*",
    ] {
        assert!(
            build.contains(expected),
            "build provenance is missing {expected:?}"
        );
    }
    for line in workflow
        .lines()
        .filter(|line| line.trim_start().starts_with("uses:"))
    {
        let reference = line
            .trim()
            .strip_prefix("uses: ")
            .expect("uses line has its action")
            .split_whitespace()
            .next()
            .expect("action reference is present");
        let digest = reference.rsplit_once('@').expect("action is pinned").1;
        assert!(
            digest.len() == 40 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "action is not pinned to a full SHA: {reference}"
        );
    }
}

#[test]
fn package_contains_binary_docs_and_matching_checksum() {
    let temp = tempfile::tempdir().expect("create temp directory");
    let source = temp.path().join("source");
    let dist = temp.path().join("dist");
    fs::create_dir_all(&source).expect("create source directory");

    let binary = temp.path().join("jira-ops");
    fs::write(&binary, b"release-binary").expect("write fake binary");
    for name in ["README.md", "LICENSE-MIT", "LICENSE-APACHE"] {
        fs::write(source.join(name), format!("{name}\n")).expect("write release document");
    }

    let output = Command::new("bash")
        .arg(package_script())
        .arg(&binary)
        .arg("0.2.0-beta.2")
        .arg("aarch64-apple-darwin")
        .arg(&source)
        .arg(&dist)
        .output()
        .expect("run package script");
    assert!(
        output.status.success(),
        "packaging failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stem = "jira-ops-v0.2.0-beta.2-aarch64-apple-darwin";
    let archive = dist.join(format!("{stem}.tar.gz"));
    let checksum = dist.join(format!("{stem}.tar.gz.sha256"));
    assert!(archive.is_file(), "release archive was not created");
    assert!(checksum.is_file(), "checksum was not created");

    let listing = Command::new("tar")
        .args(["-tzf"])
        .arg(&archive)
        .output()
        .expect("list release archive");
    assert!(listing.status.success(), "release archive is not readable");
    let listing = String::from_utf8(listing.stdout).expect("archive paths are UTF-8");
    for path in [
        format!("{stem}/jira-ops"),
        format!("{stem}/README.md"),
        format!("{stem}/LICENSE-MIT"),
        format!("{stem}/LICENSE-APACHE"),
    ] {
        assert!(listing.lines().any(|entry| entry == path), "missing {path}");
    }

    let digest = Sha256::digest(fs::read(&archive).expect("read archive"))
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let checksum_text = fs::read_to_string(checksum).expect("read checksum");
    assert_eq!(checksum_text, format!("{digest}  {stem}.tar.gz\n"));
}

#[test]
fn package_rejects_missing_release_document_without_partial_artifacts() {
    let temp = tempfile::tempdir().expect("create temp directory");
    let source = temp.path().join("source");
    let dist = temp.path().join("dist");
    fs::create_dir_all(&source).expect("create source directory");
    fs::write(source.join("README.md"), "readme\n").expect("write readme");
    fs::write(source.join("LICENSE-MIT"), "license\n").expect("write license");
    let binary = temp.path().join("jira-ops");
    fs::write(&binary, b"release-binary").expect("write fake binary");

    let output = Command::new("bash")
        .arg(package_script())
        .arg(&binary)
        .arg("0.2.0-beta.2")
        .arg("aarch64-apple-darwin")
        .arg(&source)
        .arg(&dist)
        .output()
        .expect("run package script");

    assert!(!output.status.success(), "missing license must fail closed");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("LICENSE-APACHE"),
        "failure should identify the missing input"
    );
    assert!(
        !dist.exists() || fs::read_dir(dist).expect("read dist").next().is_none(),
        "failed packaging left a partial release artifact"
    );
}
