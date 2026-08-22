use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use clap::{Command, CommandFactory, ValueEnum};
use serde::Serialize;

use crate::error::{AppError, ErrorCode, RetrySafety};

const MAX_GENERATED_FILE_BYTES: usize = 1024 * 1024;
const MAX_GENERATED_FILES: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Elvish,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ManOutput {
    pub files: Vec<PathBuf>,
}

pub fn generate_completion(shell: CompletionShell) -> Result<String, AppError> {
    let mut command = crate::cli::Cli::command();
    let mut bytes = Vec::new();
    clap_complete::generate(
        completion_generator(shell),
        &mut command,
        "jira-ops",
        &mut bytes,
    );
    if bytes.len() > MAX_GENERATED_FILE_BYTES {
        return Err(local_error("generated completion exceeds the 1 MiB limit"));
    }
    String::from_utf8(bytes).map_err(|_| local_error("generated completion is not valid UTF-8"))
}

pub fn generate_man_pages(root: &Command, output: &Path) -> Result<Vec<PathBuf>, AppError> {
    validate_output_directory(output)?;
    let mut rendered = Vec::new();
    collect_man_pages(root, "jira-ops", &mut rendered)?;
    if rendered.len() > MAX_GENERATED_FILES {
        return Err(local_error("generated man page count exceeds the limit"));
    }
    rendered.sort_by(|left, right| left.0.cmp(&right.0));

    let existed = output.exists();
    if !existed {
        fs::create_dir(output).map_err(|_| local_error("failed to create man output directory"))?;
    }
    let mut created = Vec::new();
    for (name, bytes) in &rendered {
        let path = output.join(name);
        let result = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| {
                file.write_all(bytes)?;
                file.sync_all()
            });
        if result.is_err() {
            cleanup_partial(output, &created, !existed);
            return Err(local_error("failed to write generated man pages"));
        }
        created.push(path);
    }
    Ok(rendered
        .into_iter()
        .map(|(name, _)| PathBuf::from(name))
        .collect())
}

fn completion_generator(shell: CompletionShell) -> clap_complete::Shell {
    match shell {
        CompletionShell::Bash => clap_complete::Shell::Bash,
        CompletionShell::Zsh => clap_complete::Shell::Zsh,
        CompletionShell::Fish => clap_complete::Shell::Fish,
        CompletionShell::PowerShell => clap_complete::Shell::PowerShell,
        CompletionShell::Elvish => clap_complete::Shell::Elvish,
    }
}

fn collect_man_pages(
    command: &Command,
    qualified_name: &str,
    rendered: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), AppError> {
    let mut bytes = Vec::new();
    clap_mangen::Man::new(command.clone().bin_name(qualified_name))
        .render(&mut bytes)
        .map_err(|_| local_error("failed to render man page"))?;
    if bytes.len() > MAX_GENERATED_FILE_BYTES {
        return Err(local_error("generated man page exceeds the 1 MiB limit"));
    }
    rendered.push((format!("{qualified_name}.1"), bytes));
    for child in command.get_subcommands() {
        collect_man_pages(
            child,
            &format!("{qualified_name}-{}", child.get_name()),
            rendered,
        )?;
    }
    Ok(())
}

fn validate_output_directory(output: &Path) -> Result<(), AppError> {
    if output.as_os_str().is_empty()
        || output == Path::new("/")
        || output
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(invalid_target());
    }
    if directories::BaseDirs::new().is_some_and(|directories| output == directories.home_dir()) {
        return Err(invalid_target());
    }
    if let Ok(metadata) = fs::symlink_metadata(output) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(invalid_target());
        }
        if fs::read_dir(output)
            .map_err(|_| invalid_target())?
            .next()
            .is_some()
        {
            return Err(invalid_target());
        }
    } else {
        let parent = output.parent().ok_or_else(invalid_target)?;
        let metadata = fs::metadata(parent).map_err(|_| invalid_target())?;
        if !metadata.is_dir() {
            return Err(invalid_target());
        }
    }
    Ok(())
}

fn cleanup_partial(output: &Path, created: &[PathBuf], remove_directory: bool) {
    for path in created {
        let _ = fs::remove_file(path);
    }
    if remove_directory {
        let _ = fs::remove_dir(output);
    }
}

fn invalid_target() -> AppError {
    AppError::new(
        ErrorCode::InvalidInput,
        "man output must be a safe empty directory",
        RetrySafety::Safe,
    )
}

fn local_error(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::Internal, message, RetrySafety::Safe)
}
