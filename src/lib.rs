#![allow(
    clippy::result_large_err,
    reason = "the stable AppError carries structured JSON details; the optional TOON encoder enables serde_json preserve_order and crosses Clippy's size heuristic"
)]

pub mod adf;
pub mod auth;
pub mod cli;
pub mod client;
pub mod commands;
pub mod config;
pub mod content;
pub mod cursor;
pub mod error;
pub mod model;
pub mod output;
pub mod schema;

use std::ffi::OsString;
use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::Duration;

#[cfg(not(jira_ops_hierarchy_test))]
use auth::SystemCredentialStore;
use auth::auth_status;
use cli::{
    AuthCommand, BoardCommand, Cli, Command, ConfigCommand, EpicCommand, FieldCommand,
    IssueCommand, IssueLinkCommand, IssueRemoteLinkCommand, IssueWatcherCommand,
    IssueWorklogCommand, OutputFormat, ProjectCommand, ReleaseCommand, ServerCommand,
    SprintCommand, UrlCommand, UserCommand, is_mutation_invocation, parse_error_output,
};
#[cfg(jira_ops_hierarchy_test)]
use client::HierarchyTestTransport;
#[cfg(not(jira_ops_hierarchy_test))]
use client::UreqTransport;
use client::{JiraClient, JiraTransport};
use commands::assignment::{apply_assignment, plan_assignment, validate_assignment_input};
#[cfg(not(jira_ops_hierarchy_test))]
use commands::auth::production_credentials;
use commands::auth::{auth_login, auth_logout, me_command, tenant_info};
use commands::board::board_list;
use commands::clone::{CloneIssueInput, apply_clone_issue, plan_clone_issue};
use commands::comment::{apply_comment, issue_comments, plan_comment, validate_comment_input};
use commands::destructive::{apply_delete_issue, plan_delete_issue, validate_delete_input};
use commands::epic::{
    apply_epic_add, apply_epic_create, apply_epic_remove, epic_jql, plan_epic_add,
    plan_epic_create, plan_epic_remove, validate_epic_membership, validate_epic_remove,
};
use commands::field::field_list;
use commands::issue::{
    apply_create_issue, apply_update_issue, issue_create_meta, issue_get, issue_search,
    plan_create_issue, plan_update_issue, validate_create_input, validate_update_input,
};
use commands::link::{
    apply_link, apply_remove_link, issue_link_get, issue_link_types, plan_link, plan_remove_link,
    validate_link_input,
};
use commands::local_docs::{ManOutput, generate_completion, generate_man_pages};
use commands::project::{
    apply_project_create, plan_project_create, project_get, project_list, project_templates,
    validate_project_create_input,
};
use commands::release::release_list;
use commands::remote_link::{
    apply_remote_link_add, apply_remote_link_remove, plan_remote_link_add, plan_remote_link_remove,
    remote_link_get, remote_link_list, validate_remote_link_input,
};
use commands::server::server_info;
use commands::settings::{
    ConfigPatch, ConfigUnsetInput, UrlOutput, canonical_issue_url, canonical_project_url,
    config_get, config_set, config_unset, configured_site,
};
use commands::sprint::{
    apply_sprint_add, apply_sprint_close, parse_sprint_state, plan_sprint_add, plan_sprint_close,
    sprint_list, validate_sprint_add, validate_sprint_close,
};
use commands::transition::{
    apply_transition_issue, issue_transitions, plan_transition_issue, validate_transition_input,
};
use commands::user::user_search;
use commands::watcher::{
    apply_watcher_add, apply_watcher_remove, issue_watchers, plan_watcher_add, plan_watcher_remove,
    validate_watcher_input,
};
use commands::worklog::{
    apply_worklog_add, apply_worklog_delete, apply_worklog_update, plan_worklog_add,
    plan_worklog_delete, plan_worklog_update, validate_worklog_write, worklog_list,
};
use commands::{
    authenticated_client, mutation_not_applied, read_json_input, reject_read_only_apply,
};
use config::{
    ConfigStore, CredentialSource, CredentialStore, EnvironmentSource, prepare_credential,
    resolve_prepared_credential,
};
#[cfg(jira_ops_hierarchy_test)]
use config::{CredentialKey, SavedIdentity, StoreError};
#[cfg(not(jira_ops_hierarchy_test))]
use config::{FileConfigStore, ProcessEnvironment, config_store_error};
use error::{AppError, ErrorCode, ExitClass, OperationOutcome, RetrySafety};
use model::{
    AssignmentInput, CommentInput, CreateIssueInput, DeleteIssueInput, EpicMembershipInput,
    EpicRemoveInput, LinkInput, ProjectCreateInput, RemoteLinkInput, RemoveLinkInput,
    RemoveRemoteLinkInput, SprintAddInput, SprintCloseInput, TransitionInput, UpdateIssueInput,
    WatcherInput, WorklogDeleteInput, WorklogWriteInput,
};
use output::{ErrorWriteStatus, SuccessEnvelope, write_error, write_success};
use schema::schema_for;
#[cfg(jira_ops_hierarchy_test)]
use secrecy::SecretString;
use serde::Serialize;
#[cfg(jira_ops_hierarchy_test)]
use url::Url;
#[cfg(jira_ops_hierarchy_test)]
use uuid::Uuid;

#[derive(Serialize)]
struct VersionData {
    cli_version: &'static str,
    contract_version: &'static str,
}

#[derive(Clone, Copy)]
struct OutputStyle {
    format: OutputFormat,
    pretty: bool,
}

pub trait MutationRuntime {
    type Config: ConfigStore;
    type Credentials: CredentialStore;
    type Transport: JiraTransport;

    fn config(&self) -> Result<Self::Config, AppError>;
    fn credentials(&self) -> Self::Credentials;
    fn transport(&self) -> Self::Transport;
}

#[cfg(not(jira_ops_hierarchy_test))]
struct ProductionMutationRuntime;

#[cfg(not(jira_ops_hierarchy_test))]
impl MutationRuntime for ProductionMutationRuntime {
    type Config = FileConfigStore;
    type Credentials = SystemCredentialStore;
    type Transport = UreqTransport;

    fn config(&self) -> Result<Self::Config, AppError> {
        FileConfigStore::for_current_user().map_err(config_store_error)
    }

    fn credentials(&self) -> Self::Credentials {
        production_credentials()
    }

    fn transport(&self) -> Self::Transport {
        UreqTransport
    }
}

trait ApplicationRuntime: MutationRuntime {
    type Environment: EnvironmentSource;

    fn environment(&self) -> Self::Environment;
}

#[cfg(not(jira_ops_hierarchy_test))]
impl ApplicationRuntime for ProductionMutationRuntime {
    type Environment = ProcessEnvironment;

    fn environment(&self) -> Self::Environment {
        ProcessEnvironment
    }
}

#[cfg(jira_ops_hierarchy_test)]
struct HierarchyTestEnvironment;

#[cfg(jira_ops_hierarchy_test)]
impl EnvironmentSource for HierarchyTestEnvironment {
    fn value(&self, _key: &str) -> Option<OsString> {
        None
    }
}

#[cfg(jira_ops_hierarchy_test)]
struct HierarchyTestConfig;

#[cfg(jira_ops_hierarchy_test)]
impl ConfigStore for HierarchyTestConfig {
    fn load(&self) -> Result<Option<SavedIdentity>, StoreError> {
        Ok(Some(SavedIdentity {
            site: Url::parse("https://example.atlassian.net/").expect("static hierarchy test site"),
            cloud_id: Uuid::nil(),
            email: "hierarchy-agent@example.invalid".to_owned(),
            account_id: "hierarchy-test-account".to_owned(),
            default_project: None,
            default_board: None,
        }))
    }

    fn atomic_replace(&self, _value: &SavedIdentity) -> Result<(), StoreError> {
        panic!("hierarchy test runtime forbids config writes")
    }

    fn remove(&self) -> Result<(), StoreError> {
        panic!("hierarchy test runtime forbids config deletion")
    }
}

#[cfg(jira_ops_hierarchy_test)]
struct HierarchyTestCredentials;

#[cfg(jira_ops_hierarchy_test)]
impl CredentialStore for HierarchyTestCredentials {
    fn get(&self, _key: &CredentialKey) -> Result<SecretString, StoreError> {
        Ok(SecretString::from("hierarchy-static-test-token"))
    }

    fn set(&self, _key: &CredentialKey, _value: &SecretString) -> Result<(), StoreError> {
        panic!("hierarchy test runtime forbids credential writes")
    }

    fn delete(&self, _key: &CredentialKey) -> Result<(), StoreError> {
        panic!("hierarchy test runtime forbids credential deletion")
    }
}

#[cfg(jira_ops_hierarchy_test)]
struct HierarchyTestRuntime;

#[cfg(jira_ops_hierarchy_test)]
impl MutationRuntime for HierarchyTestRuntime {
    type Config = HierarchyTestConfig;
    type Credentials = HierarchyTestCredentials;
    type Transport = HierarchyTestTransport;

    fn config(&self) -> Result<Self::Config, AppError> {
        Ok(HierarchyTestConfig)
    }

    fn credentials(&self) -> Self::Credentials {
        HierarchyTestCredentials
    }

    fn transport(&self) -> Self::Transport {
        HierarchyTestTransport::from_process()
    }
}

#[cfg(jira_ops_hierarchy_test)]
impl ApplicationRuntime for HierarchyTestRuntime {
    type Environment = HierarchyTestEnvironment;

    fn environment(&self) -> Self::Environment {
        HierarchyTestEnvironment
    }
}

pub fn run_main<I>(
    args: I,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = args.into_iter().collect();
    let (parse_error_format, parse_error_pretty) = parse_error_output(&args);
    let mutation_requested = is_mutation_invocation(&args);

    let exit = match Cli::parse_args(args) {
        Ok(cli) => {
            #[cfg(not(jira_ops_hierarchy_test))]
            let runtime = ProductionMutationRuntime;
            #[cfg(jira_ops_hierarchy_test)]
            let runtime = HierarchyTestRuntime;
            match dispatch(
                cli.command,
                stdin,
                stdout,
                OutputStyle {
                    format: cli.output,
                    pretty: cli.pretty,
                },
                Duration::from_millis(cli.timeout_ms),
                &runtime,
            ) {
                Ok(()) => ExitClass::Success,
                Err(error) => write_app_error(stderr, &error, cli.output, cli.pretty),
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            match write!(stdout, "{}", error.render()) {
                Ok(()) => ExitClass::Success,
                Err(error) => write_app_error(
                    stderr,
                    &output_failure(error),
                    parse_error_format,
                    parse_error_pretty,
                ),
            }
        }
        Err(_) => {
            let mut error = AppError::new(
                ErrorCode::InvalidInput,
                "invalid command syntax",
                RetrySafety::Safe,
            );
            if mutation_requested {
                error = mutation_not_applied(error);
            }
            write_app_error(stderr, &error, parse_error_format, parse_error_pretty)
        }
    };

    ExitCode::from(exit.code())
}

#[doc(hidden)]
pub fn run_mutation_with_runtime<I, E, R>(
    args: I,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    environment: &E,
    runtime: &R,
) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
    E: EnvironmentSource,
    R: MutationRuntime,
{
    let args: Vec<OsString> = args.into_iter().collect();
    let (parse_error_format, parse_error_pretty) = parse_error_output(&args);
    let exit = match Cli::parse_args(args) {
        Ok(cli) => match cli.command {
            Command::Issue(args)
                if matches!(
                    args.command,
                    IssueCommand::Create(_)
                        | IssueCommand::Clone(_)
                        | IssueCommand::Delete(_)
                        | IssueCommand::Update(_)
                        | IssueCommand::Comment(_)
                        | IssueCommand::Transition(_)
                        | IssueCommand::Assign(_)
                        | IssueCommand::Link(cli::IssueLinkArgs {
                            command: IssueLinkCommand::Add(_) | IssueLinkCommand::Remove(_)
                        })
                        | IssueCommand::RemoteLink(cli::IssueRemoteLinkArgs {
                            command: IssueRemoteLinkCommand::Add(_)
                                | IssueRemoteLinkCommand::Remove(_)
                        })
                        | IssueCommand::Worklog(cli::IssueWorklogArgs {
                            command: IssueWorklogCommand::Add(_)
                                | IssueWorklogCommand::Update(_)
                                | IssueWorklogCommand::Delete(_)
                        })
                        | IssueCommand::Watcher(cli::IssueWatcherArgs {
                            command: IssueWatcherCommand::Add(_) | IssueWatcherCommand::Remove(_)
                        })
                ) =>
            {
                dispatch_mutation(
                    args.command,
                    stdin,
                    stdout,
                    OutputStyle {
                        format: cli.output,
                        pretty: cli.pretty,
                    },
                    Duration::from_millis(cli.timeout_ms),
                    environment,
                    runtime,
                )
            }
            Command::Project(args) if matches!(args.command, ProjectCommand::Create(_)) => {
                dispatch_project_mutation(
                    args.command,
                    stdin,
                    stdout,
                    OutputStyle {
                        format: cli.output,
                        pretty: cli.pretty,
                    },
                    Duration::from_millis(cli.timeout_ms),
                    environment,
                    runtime,
                )
            }
            Command::Epic(args)
                if matches!(
                    args.command,
                    EpicCommand::Create(_) | EpicCommand::Add(_) | EpicCommand::Remove(_)
                ) =>
            {
                dispatch_epic_mutation(
                    args.command,
                    stdin,
                    stdout,
                    OutputStyle {
                        format: cli.output,
                        pretty: cli.pretty,
                    },
                    Duration::from_millis(cli.timeout_ms),
                    environment,
                    runtime,
                )
            }
            Command::Sprint(args)
                if matches!(
                    args.command,
                    SprintCommand::Add(_) | SprintCommand::Close(_)
                ) =>
            {
                dispatch_sprint_mutation(
                    args.command,
                    stdin,
                    stdout,
                    OutputStyle {
                        format: cli.output,
                        pretty: cli.pretty,
                    },
                    Duration::from_millis(cli.timeout_ms),
                    environment,
                    runtime,
                )
            }
            _ => Err(mutation_not_applied(AppError::new(
                ErrorCode::InvalidInput,
                "expected a mutation command",
                RetrySafety::Safe,
            ))),
        },
        Err(_) => Err(mutation_not_applied(AppError::new(
            ErrorCode::InvalidInput,
            "invalid command syntax",
            RetrySafety::Safe,
        ))),
    };
    let exit = match exit {
        Ok(()) => ExitClass::Success,
        Err(error) => write_app_error(stderr, &error, parse_error_format, parse_error_pretty),
    };
    ExitCode::from(exit.code())
}

fn dispatch<R: ApplicationRuntime>(
    command: Command,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    output: OutputStyle,
    timeout: Duration,
    runtime: &R,
) -> Result<(), AppError> {
    match command {
        Command::Version => write_success(
            stdout,
            &SuccessEnvelope::new(VersionData {
                cli_version: env!("CARGO_PKG_VERSION"),
                contract_version: "1",
            }),
            output.format,
            output.pretty,
        )
        .map_err(output_failure),
        Command::Schema(args) => {
            let data = schema_for(&args.path, args.all)?;
            write_success(
                stdout,
                &SuccessEnvelope::new(data),
                output.format,
                output.pretty,
            )
            .map_err(output_failure)
        }
        Command::Config(args) => {
            let config = runtime.config()?;
            let data = match args.command {
                ConfigCommand::Get => config_get(&config)?,
                ConfigCommand::Set(_) | ConfigCommand::Unset(_) => {
                    return dispatch_local_config(args.command, stdin, stdout, output, &config);
                }
            };
            write_success(
                stdout,
                &SuccessEnvelope::new(data),
                output.format,
                output.pretty,
            )
            .map_err(output_failure)
        }
        Command::Url(args) => {
            let config = runtime.config()?;
            let site = configured_site(&config)?;
            let url = match args.command {
                UrlCommand::Issue(args) => canonical_issue_url(&site, &args.issue)?,
                UrlCommand::Project(args) => canonical_project_url(&site, &args.project)?,
            };
            write_success(
                stdout,
                &SuccessEnvelope::new(UrlOutput { url }),
                output.format,
                output.pretty,
            )
            .map_err(output_failure)
        }
        Command::Completion(args) => write_success(
            stdout,
            &SuccessEnvelope::new(generate_completion(args.shell)?),
            output.format,
            output.pretty,
        )
        .map_err(output_failure),
        Command::Man(args) => {
            let files =
                generate_man_pages(&<Cli as clap::CommandFactory>::command(), &args.output_dir)?;
            write_success(
                stdout,
                &SuccessEnvelope::new(ManOutput { files }),
                output.format,
                output.pretty,
            )
            .map_err(output_failure)
        }
        Command::Server(args) => {
            let environment = runtime.environment();
            let config = runtime.config()?;
            let credentials = runtime.credentials();
            let transport = runtime.transport();
            let client =
                authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
            match args.command {
                ServerCommand::Info => {
                    write_success(stdout, &server_info(&client)?, output.format, output.pretty)
                        .map_err(output_failure)
                }
            }
        }
        Command::User(args) => {
            let environment = runtime.environment();
            let config = runtime.config()?;
            let credentials = runtime.credentials();
            let transport = runtime.transport();
            let client =
                authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
            match args.command {
                UserCommand::Search(args) => write_success(
                    stdout,
                    &user_search(
                        &client,
                        &args.query,
                        args.page.limit,
                        args.page.cursor.as_deref(),
                    )?,
                    output.format,
                    output.pretty,
                )
                .map_err(output_failure),
            }
        }
        Command::Board(args) => {
            let environment = runtime.environment();
            let config = runtime.config()?;
            let credentials = runtime.credentials();
            let transport = runtime.transport();
            let client =
                authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
            match args.command {
                BoardCommand::List(args) => write_success(
                    stdout,
                    &board_list(
                        &client,
                        args.project.as_deref(),
                        args.board_type.as_deref(),
                        args.page.limit,
                        args.page.cursor.as_deref(),
                    )?,
                    output.format,
                    output.pretty,
                )
                .map_err(output_failure),
            }
        }
        Command::Release(args) => {
            let environment = runtime.environment();
            let config = runtime.config()?;
            let credentials = runtime.credentials();
            let transport = runtime.transport();
            let client =
                authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
            match args.command {
                ReleaseCommand::List(args) => write_success(
                    stdout,
                    &release_list(
                        &client,
                        &args.project,
                        args.status.as_deref(),
                        args.page.limit,
                        args.page.cursor.as_deref(),
                    )?,
                    output.format,
                    output.pretty,
                )
                .map_err(output_failure),
            }
        }
        Command::Auth(args) => {
            let environment = runtime.environment();
            let config = runtime.config()?;
            match args.command {
                AuthCommand::Status => {
                    let data = auth_status(&environment, &config)?;
                    write_success(
                        stdout,
                        &SuccessEnvelope::new(data),
                        output.format,
                        output.pretty,
                    )
                    .map_err(output_failure)
                }
                AuthCommand::Login(args) => {
                    let credentials = runtime.credentials();
                    let transport = runtime.transport();
                    let result = auth_login(
                        &environment,
                        &config,
                        &credentials,
                        &transport,
                        &args.site,
                        &args.email,
                        stdin,
                        timeout,
                    )?;
                    let mut envelope = SuccessEnvelope::new(result.data);
                    envelope.warnings = result.warnings;
                    write_success(stdout, &envelope, output.format, output.pretty)
                        .map_err(output_failure)
                }
                AuthCommand::Logout => {
                    let credentials = runtime.credentials();
                    let data = auth_logout(&environment, &config, &credentials)?;
                    write_success(
                        stdout,
                        &SuccessEnvelope::new(data),
                        output.format,
                        output.pretty,
                    )
                    .map_err(output_failure)
                }
            }
        }
        Command::Me => {
            let environment = runtime.environment();
            let config = runtime.config()?;
            let credentials = runtime.credentials();
            let transport = runtime.transport();
            let data = me_command(&environment, &config, &credentials, &transport, timeout)?;
            write_success(
                stdout,
                &SuccessEnvelope::new(data),
                output.format,
                output.pretty,
            )
            .map_err(output_failure)
        }
        Command::Project(args) => match args.command {
            ProjectCommand::Templates(args) => {
                let envelope = project_templates(args.project_type.as_deref());
                write_success(stdout, &envelope, output.format, output.pretty)
                    .map_err(output_failure)
            }
            command @ ProjectCommand::Create(_) => {
                let environment = runtime.environment();
                dispatch_project_mutation(
                    command,
                    stdin,
                    stdout,
                    output,
                    timeout,
                    &environment,
                    runtime,
                )
            }
            command @ (ProjectCommand::List(_) | ProjectCommand::Get(_)) => {
                let environment = runtime.environment();
                let config = runtime.config()?;
                let credentials = runtime.credentials();
                let transport = runtime.transport();
                let client =
                    authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
                match command {
                    ProjectCommand::List(page) => {
                        let envelope = project_list(&client, page.limit, page.cursor.as_deref())?;
                        write_success(stdout, &envelope, output.format, output.pretty)
                            .map_err(output_failure)
                    }
                    ProjectCommand::Get(args) => {
                        let envelope = project_get(&client, &args.project)?;
                        write_success(stdout, &envelope, output.format, output.pretty)
                            .map_err(output_failure)
                    }
                    ProjectCommand::Templates(_) | ProjectCommand::Create(_) => unreachable!(),
                }
            }
        },
        Command::Epic(args) => match args.command {
            command @ (EpicCommand::Create(_) | EpicCommand::Add(_) | EpicCommand::Remove(_)) => {
                let environment = runtime.environment();
                dispatch_epic_mutation(
                    command,
                    stdin,
                    stdout,
                    output,
                    timeout,
                    &environment,
                    runtime,
                )
            }
            EpicCommand::List(args) => {
                let environment = runtime.environment();
                let config = runtime.config()?;
                let credentials = runtime.credentials();
                let transport = runtime.transport();
                let client =
                    authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
                let jql = epic_jql(&args.project, args.jql.as_deref())?;
                write_success(
                    stdout,
                    &issue_search(
                        &client,
                        &jql,
                        args.fields.as_ref().map(|fields| fields.as_slice()),
                        args.page.limit,
                        args.page.cursor.as_deref(),
                    )?,
                    output.format,
                    output.pretty,
                )
                .map_err(output_failure)
            }
        },
        Command::Sprint(args) => match args.command {
            command @ (SprintCommand::Add(_) | SprintCommand::Close(_)) => {
                let environment = runtime.environment();
                dispatch_sprint_mutation(
                    command,
                    stdin,
                    stdout,
                    output,
                    timeout,
                    &environment,
                    runtime,
                )
            }
            SprintCommand::List(args) => {
                let environment = runtime.environment();
                let config = runtime.config()?;
                let credentials = runtime.credentials();
                let transport = runtime.transport();
                let client =
                    authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
                write_success(
                    stdout,
                    &sprint_list(
                        &client,
                        args.board,
                        parse_sprint_state(args.state.as_deref())?,
                        args.page.limit,
                        args.page.cursor.as_deref(),
                    )?,
                    output.format,
                    output.pretty,
                )
                .map_err(output_failure)
            }
            SprintCommand::Issues(args) => {
                let environment = runtime.environment();
                let config = runtime.config()?;
                let credentials = runtime.credentials();
                let transport = runtime.transport();
                let client =
                    authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
                let jql = format!("sprint = {}", args.sprint_id);
                write_success(
                    stdout,
                    &issue_search(
                        &client,
                        &jql,
                        args.fields.as_ref().map(|v| v.as_slice()),
                        args.page.limit,
                        args.page.cursor.as_deref(),
                    )?,
                    output.format,
                    output.pretty,
                )
                .map_err(output_failure)
            }
        },
        Command::Field(args) => {
            let environment = runtime.environment();
            let config = runtime.config()?;
            let credentials = runtime.credentials();
            let transport = runtime.transport();
            let client =
                authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
            match args.command {
                FieldCommand::List(args) => {
                    let envelope = field_list(
                        &client,
                        args.query.as_deref(),
                        args.page.limit,
                        args.page.cursor.as_deref(),
                    )?;
                    write_success(stdout, &envelope, output.format, output.pretty)
                        .map_err(output_failure)
                }
            }
        }
        Command::Issue(args) => match args.command {
            command @ (IssueCommand::Create(_)
            | IssueCommand::Clone(_)
            | IssueCommand::Delete(_)
            | IssueCommand::Update(_)
            | IssueCommand::Comment(_)
            | IssueCommand::Transition(_)
            | IssueCommand::Assign(_)
            | IssueCommand::Link(cli::IssueLinkArgs {
                command: IssueLinkCommand::Add(_) | IssueLinkCommand::Remove(_),
            })
            | IssueCommand::RemoteLink(cli::IssueRemoteLinkArgs {
                command: IssueRemoteLinkCommand::Add(_) | IssueRemoteLinkCommand::Remove(_),
            })
            | IssueCommand::Worklog(cli::IssueWorklogArgs {
                command:
                    IssueWorklogCommand::Add(_)
                    | IssueWorklogCommand::Update(_)
                    | IssueWorklogCommand::Delete(_),
            })
            | IssueCommand::Watcher(cli::IssueWatcherArgs {
                command: IssueWatcherCommand::Add(_) | IssueWatcherCommand::Remove(_),
            })) => {
                let environment = runtime.environment();
                dispatch_mutation(
                    command,
                    stdin,
                    stdout,
                    output,
                    timeout,
                    &environment,
                    runtime,
                )
            }
            IssueCommand::Get(args) => {
                let environment = runtime.environment();
                let config = runtime.config()?;
                let credentials = runtime.credentials();
                let transport = runtime.transport();
                let client =
                    authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
                let envelope = issue_get(
                    &client,
                    &args.issue,
                    args.fields.as_ref().map(|fields| fields.as_slice()),
                )?;
                write_success(stdout, &envelope, output.format, output.pretty)
                    .map_err(output_failure)
            }
            IssueCommand::Search(args) => {
                let environment = runtime.environment();
                let config = runtime.config()?;
                let credentials = runtime.credentials();
                let transport = runtime.transport();
                let client =
                    authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
                let envelope = issue_search(
                    &client,
                    &args.jql,
                    args.fields.as_ref().map(|fields| fields.as_slice()),
                    args.page.limit,
                    args.page.cursor.as_deref(),
                )?;
                write_success(stdout, &envelope, output.format, output.pretty)
                    .map_err(output_failure)
            }
            IssueCommand::CreateMeta(args) => {
                let environment = runtime.environment();
                let config = runtime.config()?;
                let credentials = runtime.credentials();
                let transport = runtime.transport();
                let client =
                    authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
                let envelope = issue_create_meta(
                    &client,
                    &args.project,
                    args.issue_type.as_deref(),
                    args.page.limit,
                    args.page.cursor.as_deref(),
                )?;
                write_success(stdout, &envelope, output.format, output.pretty)
                    .map_err(output_failure)
            }
            IssueCommand::Comments(args) => {
                let environment = runtime.environment();
                let config = runtime.config()?;
                let credentials = runtime.credentials();
                let transport = runtime.transport();
                let client =
                    authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
                let envelope = issue_comments(
                    &client,
                    &args.issue,
                    args.page.limit,
                    args.page.cursor.as_deref(),
                )?;
                write_success(stdout, &envelope, output.format, output.pretty)
                    .map_err(output_failure)
            }
            IssueCommand::Transitions(args) => {
                let environment = runtime.environment();
                let config = runtime.config()?;
                let credentials = runtime.credentials();
                let transport = runtime.transport();
                let client =
                    authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
                let envelope = issue_transitions(&client, &args.issue)?;
                write_success(stdout, &envelope, output.format, output.pretty)
                    .map_err(output_failure)
            }
            IssueCommand::Link(args) => {
                let environment = runtime.environment();
                let config = runtime.config()?;
                let credentials = runtime.credentials();
                let transport = runtime.transport();
                let client =
                    authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
                match args.command {
                    IssueLinkCommand::Types => write_success(
                        stdout,
                        &issue_link_types(&client)?,
                        output.format,
                        output.pretty,
                    )
                    .map_err(output_failure),
                    IssueLinkCommand::Get(args) => write_success(
                        stdout,
                        &issue_link_get(&client, &args.link_id)?,
                        output.format,
                        output.pretty,
                    )
                    .map_err(output_failure),
                    IssueLinkCommand::Add(_) | IssueLinkCommand::Remove(_) => Err(AppError::new(
                        ErrorCode::Internal,
                        "issue link mutation escaped mutation dispatch",
                        RetrySafety::Safe,
                    )),
                }
            }
            IssueCommand::RemoteLink(args) => {
                let environment = runtime.environment();
                let config = runtime.config()?;
                let credentials = runtime.credentials();
                let transport = runtime.transport();
                let client =
                    authenticated_client(&environment, &config, &credentials, &transport, timeout)?;
                match args.command {
                    IssueRemoteLinkCommand::List(args) => write_success(
                        stdout,
                        &remote_link_list(&client, &args.issue)?,
                        output.format,
                        output.pretty,
                    )
                    .map_err(output_failure),
                    IssueRemoteLinkCommand::Get(args) => write_success(
                        stdout,
                        &remote_link_get(&client, &args.issue, &args.remote_link_id)?,
                        output.format,
                        output.pretty,
                    )
                    .map_err(output_failure),
                    IssueRemoteLinkCommand::Add(_) | IssueRemoteLinkCommand::Remove(_) => {
                        Err(AppError::new(
                            ErrorCode::Internal,
                            "remote-link mutation escaped mutation dispatch",
                            RetrySafety::Safe,
                        ))
                    }
                }
            }
            IssueCommand::Worklog(args) => match args.command {
                IssueWorklogCommand::List(args) => {
                    let environment = runtime.environment();
                    let config = runtime.config()?;
                    let credentials = runtime.credentials();
                    let transport = runtime.transport();
                    let client = authenticated_client(
                        &environment,
                        &config,
                        &credentials,
                        &transport,
                        timeout,
                    )?;
                    write_success(
                        stdout,
                        &worklog_list(
                            &client,
                            &args.issue,
                            args.page.limit,
                            args.page.cursor.as_deref(),
                        )?,
                        output.format,
                        output.pretty,
                    )
                    .map_err(output_failure)
                }
                IssueWorklogCommand::Add(_)
                | IssueWorklogCommand::Update(_)
                | IssueWorklogCommand::Delete(_) => Err(AppError::new(
                    ErrorCode::Internal,
                    "worklog mutation escaped mutation dispatch",
                    RetrySafety::Safe,
                )),
            },
            IssueCommand::Watcher(args) => match args.command {
                IssueWatcherCommand::List(args) => {
                    let environment = runtime.environment();
                    let config = runtime.config()?;
                    let credentials = runtime.credentials();
                    let transport = runtime.transport();
                    let client = authenticated_client(
                        &environment,
                        &config,
                        &credentials,
                        &transport,
                        timeout,
                    )?;
                    write_success(
                        stdout,
                        &issue_watchers(&client, &args.issue)?,
                        output.format,
                        output.pretty,
                    )
                    .map_err(output_failure)
                }
                IssueWatcherCommand::Add(_) | IssueWatcherCommand::Remove(_) => Err(AppError::new(
                    ErrorCode::Internal,
                    "watcher mutation escaped mutation dispatch",
                    RetrySafety::Safe,
                )),
            },
        },
    }
}

fn dispatch_local_config(
    command: ConfigCommand,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    output: OutputStyle,
    config: &impl ConfigStore,
) -> Result<(), AppError> {
    let data = match command {
        ConfigCommand::Set(_) => {
            let input: ConfigPatch = read_json_input(stdin)?;
            config_set(config, input)?
        }
        ConfigCommand::Unset(_) => {
            let input: ConfigUnsetInput = read_json_input(stdin)?;
            config_unset(config, input)?
        }
        ConfigCommand::Get => {
            return Err(AppError::new(
                ErrorCode::Internal,
                "config get escaped local dispatch",
                RetrySafety::Safe,
            ));
        }
    };
    write_success(
        stdout,
        &SuccessEnvelope::new(data),
        output.format,
        output.pretty,
    )
    .map_err(output_failure)
}

fn dispatch_mutation<E: EnvironmentSource, R: MutationRuntime>(
    command: IssueCommand,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    output: OutputStyle,
    timeout: Duration,
    environment: &E,
    runtime: &R,
) -> Result<(), AppError> {
    match command {
        IssueCommand::Create(args) => {
            let input: CreateIssueInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_create_input(&input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let client = mutation_client(environment, runtime, timeout)?;
            let plan = plan_create_issue(&client, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let applied = apply_create_issue(&client, plan)?;
            write_applied(stdout, applied, output)
        }
        IssueCommand::Clone(args) => {
            let input: CloneIssueInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let client = mutation_client(environment, runtime, timeout)?;
            let plan =
                plan_clone_issue(&client, &args.issue, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let applied = apply_clone_issue(&client, plan)?;
            write_applied(stdout, applied, output)
        }
        IssueCommand::Delete(args) => {
            let input: DeleteIssueInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_delete_input(&args.issue, &input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let plan = plan_delete_issue(&args.issue, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let client = mutation_client(environment, runtime, timeout)?;
            let applied = apply_delete_issue(&client, &args.issue, plan)?;
            write_applied(stdout, applied, output)
        }
        IssueCommand::Update(args) => {
            let input: UpdateIssueInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_update_input(&args.issue, &input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let client = mutation_client(environment, runtime, timeout)?;
            let plan =
                plan_update_issue(&client, &args.issue, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let applied = apply_update_issue(&client, &args.issue, plan)?;
            write_applied(stdout, applied, output)
        }
        IssueCommand::Comment(args) => {
            let input: CommentInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_comment_input(&args.issue, &input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let plan = plan_comment(&args.issue, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let client = mutation_client(environment, runtime, timeout)?;
            let applied = apply_comment(&client, &args.issue, plan)?;
            write_applied(stdout, applied, output)
        }
        IssueCommand::Transition(args) => {
            let input: TransitionInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_transition_input(&args.issue, &input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let client = mutation_client(environment, runtime, timeout)?;
            let plan =
                plan_transition_issue(&client, &args.issue, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let applied = apply_transition_issue(&client, &args.issue, plan)?;
            write_applied(stdout, applied, output)
        }
        IssueCommand::Assign(args) => {
            let input: AssignmentInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_assignment_input(&input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let plan = plan_assignment(input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let client = mutation_client(environment, runtime, timeout)?;
            let applied = apply_assignment(&client, plan)?;
            write_applied(stdout, applied, output)
        }
        IssueCommand::Link(cli::IssueLinkArgs {
            command: IssueLinkCommand::Add(args),
        }) => {
            let input: LinkInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_link_input(&input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let plan = plan_link(input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let client = mutation_client(environment, runtime, timeout)?;
            let applied = apply_link(&client, plan)?;
            write_applied(stdout, applied, output)
        }
        IssueCommand::Link(cli::IssueLinkArgs {
            command: IssueLinkCommand::Remove(args),
        }) => {
            let input: RemoveLinkInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let client = mutation_client(environment, runtime, timeout)?;
            let plan =
                plan_remove_link(&client, &args.link_id, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let applied = apply_remove_link(&client, &args.link_id, plan)?;
            write_applied(stdout, applied, output)
        }
        IssueCommand::RemoteLink(cli::IssueRemoteLinkArgs {
            command: IssueRemoteLinkCommand::Add(args),
        }) => {
            let input: RemoteLinkInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_remote_link_input(&input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let plan = plan_remote_link_add(&args.issue, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let client = mutation_client(environment, runtime, timeout)?;
            let applied = apply_remote_link_add(&client, &args.issue, plan)?;
            write_applied(stdout, applied, output)
        }
        IssueCommand::Worklog(cli::IssueWorklogArgs {
            command: IssueWorklogCommand::Add(args),
        }) => {
            let input: WorklogWriteInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_worklog_write(&input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let adjustment = input.adjustment.clone();
            let notify = input.notify_users;
            let plan = plan_worklog_add(&args.issue, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let client = mutation_client(environment, runtime, timeout)?;
            write_applied(
                stdout,
                apply_worklog_add(&client, &args.issue, &adjustment, notify, plan)?,
                output,
            )
        }
        IssueCommand::Worklog(cli::IssueWorklogArgs {
            command: IssueWorklogCommand::Update(args),
        }) => {
            let input: WorklogWriteInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_worklog_write(&input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let adjustment = input.adjustment.clone();
            let notify = input.notify_users;
            let plan = plan_worklog_update(&args.issue, &args.worklog_id, input)
                .map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let client = mutation_client(environment, runtime, timeout)?;
            write_applied(
                stdout,
                apply_worklog_update(
                    &client,
                    &args.issue,
                    &args.worklog_id,
                    &adjustment,
                    notify,
                    plan,
                )?,
                output,
            )
        }
        IssueCommand::Worklog(cli::IssueWorklogArgs {
            command: IssueWorklogCommand::Delete(args),
        }) => {
            let input: WorklogDeleteInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let adjustment = input.adjustment.clone();
            let notify = input.notify_users;
            let plan = plan_worklog_delete(&args.issue, &args.worklog_id, input)
                .map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let client = mutation_client(environment, runtime, timeout)?;
            write_applied(
                stdout,
                apply_worklog_delete(
                    &client,
                    &args.issue,
                    &args.worklog_id,
                    &adjustment,
                    notify,
                    plan,
                )?,
                output,
            )
        }
        IssueCommand::RemoteLink(cli::IssueRemoteLinkArgs {
            command: IssueRemoteLinkCommand::Remove(args),
        }) => {
            let input: RemoveRemoteLinkInput =
                read_json_input(stdin).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let client = mutation_client(environment, runtime, timeout)?;
            let plan = plan_remote_link_remove(&client, &args.issue, &args.remote_link_id, input)
                .map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let applied =
                apply_remote_link_remove(&client, &args.issue, &args.remote_link_id, plan)?;
            write_applied(stdout, applied, output)
        }
        IssueCommand::Watcher(cli::IssueWatcherArgs {
            command: IssueWatcherCommand::Add(args),
        }) => {
            let input: WatcherInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_watcher_input(&input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let plan = plan_watcher_add(input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let client = mutation_client(environment, runtime, timeout)?;
            let applied = apply_watcher_add(&client, plan)?;
            write_applied(stdout, applied, output)
        }
        IssueCommand::Watcher(cli::IssueWatcherArgs {
            command: IssueWatcherCommand::Remove(args),
        }) => {
            let input: WatcherInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_watcher_input(&input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let plan = plan_watcher_remove(input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let client = mutation_client(environment, runtime, timeout)?;
            let applied = apply_watcher_remove(&client, plan)?;
            write_applied(stdout, applied, output)
        }
        _ => Err(mutation_not_applied(AppError::new(
            ErrorCode::Internal,
            "non-mutation reached mutation dispatch",
            RetrySafety::Safe,
        ))),
    }
}

fn mutation_client<E: EnvironmentSource, R: MutationRuntime>(
    environment: &E,
    runtime: &R,
    timeout: Duration,
) -> Result<JiraClient<R::Transport>, AppError> {
    let config = runtime.config().map_err(mutation_not_applied)?;
    let prepared = prepare_credential(environment, &config).map_err(mutation_not_applied)?;
    let credentials = runtime.credentials();
    let credential =
        resolve_prepared_credential(prepared, &credentials).map_err(mutation_not_applied)?;
    let transport = runtime.transport();
    if credential.source == CredentialSource::Environment {
        let bound_cloud_id =
            tenant_info(&transport, &credential.site, timeout).map_err(mutation_not_applied)?;
        if bound_cloud_id != credential.cloud_id {
            return Err(mutation_not_applied(AppError::new(
                ErrorCode::ConfigConflict,
                "JIRA_SITE does not resolve to JIRA_CLOUD_ID",
                RetrySafety::Safe,
            )));
        }
    }
    Ok(JiraClient::new(transport, credential, timeout))
}

fn dispatch_project_mutation<E: EnvironmentSource, R: MutationRuntime>(
    command: ProjectCommand,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    output: OutputStyle,
    timeout: Duration,
    environment: &E,
    runtime: &R,
) -> Result<(), AppError> {
    let ProjectCommand::Create(args) = command else {
        return Err(mutation_not_applied(AppError::new(
            ErrorCode::Internal,
            "non-mutation reached project mutation dispatch",
            RetrySafety::Safe,
        )));
    };
    let input: ProjectCreateInput = read_json_input(stdin).map_err(mutation_not_applied)?;
    validate_project_create_input(&input).map_err(mutation_not_applied)?;
    reject_read_only_apply(environment, args.mutation.apply).map_err(mutation_not_applied)?;
    let plan = plan_project_create(input).map_err(mutation_not_applied)?;
    if !args.mutation.apply {
        return write_dry_run(stdout, plan, output);
    }
    let client = mutation_client(environment, runtime, timeout)?;
    let applied = apply_project_create(&client, plan)?;
    write_applied(stdout, applied, output)
}

fn dispatch_epic_mutation<E: EnvironmentSource, R: MutationRuntime>(
    command: EpicCommand,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    output: OutputStyle,
    timeout: Duration,
    environment: &E,
    runtime: &R,
) -> Result<(), AppError> {
    match command {
        EpicCommand::Create(args) => {
            let input: CreateIssueInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_create_input(&input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let client = mutation_client(environment, runtime, timeout)?;
            let plan = plan_epic_create(&client, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            write_applied(stdout, apply_epic_create(&client, plan)?, output)
        }
        EpicCommand::Add(args) => {
            let input: EpicMembershipInput =
                read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_epic_membership(&input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let notify = input.notify_users;
            let plan = plan_epic_add(&args.epic, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let client = mutation_client(environment, runtime, timeout)?;
            write_applied(
                stdout,
                apply_epic_add(&client, &args.epic, notify, plan)?,
                output,
            )
        }
        EpicCommand::Remove(args) => {
            let input: EpicRemoveInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_epic_remove(&args.epic, &input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let notify = input.notify_users;
            let plan = plan_epic_remove(&args.epic, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let client = mutation_client(environment, runtime, timeout)?;
            write_applied(
                stdout,
                apply_epic_remove(&client, &args.epic, notify, plan)?,
                output,
            )
        }
        EpicCommand::List(_) => Err(mutation_not_applied(AppError::new(
            ErrorCode::Internal,
            "epic list reached mutation dispatch",
            RetrySafety::Safe,
        ))),
    }
}

fn dispatch_sprint_mutation<E: EnvironmentSource, R: MutationRuntime>(
    command: SprintCommand,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    output: OutputStyle,
    timeout: Duration,
    environment: &E,
    runtime: &R,
) -> Result<(), AppError> {
    match command {
        SprintCommand::Add(args) => {
            let input: SprintAddInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_sprint_add(&input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let plan = plan_sprint_add(args.sprint_id, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            let client = mutation_client(environment, runtime, timeout)?;
            write_applied(
                stdout,
                apply_sprint_add(&client, args.sprint_id, plan)?,
                output,
            )
        }
        SprintCommand::Close(args) => {
            let input: SprintCloseInput = read_json_input(stdin).map_err(mutation_not_applied)?;
            validate_sprint_close(args.sprint_id, &input).map_err(mutation_not_applied)?;
            reject_read_only_apply(environment, args.mutation.apply)
                .map_err(mutation_not_applied)?;
            let client = mutation_client(environment, runtime, timeout)?;
            let plan =
                plan_sprint_close(&client, args.sprint_id, input).map_err(mutation_not_applied)?;
            if !args.mutation.apply {
                return write_dry_run(stdout, plan, output);
            }
            write_applied(
                stdout,
                apply_sprint_close(&client, args.sprint_id, plan)?,
                output,
            )
        }
        SprintCommand::List(_) | SprintCommand::Issues(_) => {
            Err(mutation_not_applied(AppError::new(
                ErrorCode::Internal,
                "sprint read reached mutation dispatch",
                RetrySafety::Safe,
            )))
        }
    }
}

fn write_dry_run<T: Serialize>(
    stdout: &mut dyn Write,
    plan: T,
    output: OutputStyle,
) -> Result<(), AppError> {
    write_success(
        stdout,
        &SuccessEnvelope::new(plan),
        output.format,
        output.pretty,
    )
    .map_err(output_failure)
    .map_err(mutation_not_applied)
}

fn write_applied<T: Serialize>(
    stdout: &mut dyn Write,
    applied: T,
    output: OutputStyle,
) -> Result<(), AppError> {
    write_success(
        stdout,
        &SuccessEnvelope::new(applied),
        output.format,
        output.pretty,
    )
    .map_err(applied_output_failure)
}

fn applied_output_failure(_error: std::io::Error) -> AppError {
    let mut error = AppError::new(
        ErrorCode::MutationResponseInvalid,
        "Jira applied the mutation but the CLI could not write the success output",
        RetrySafety::Unsafe,
    );
    error.operation_outcome = Some(OperationOutcome::Applied);
    error
}

fn write_app_error(
    stderr: &mut dyn Write,
    error: &AppError,
    format: OutputFormat,
    pretty: bool,
) -> ExitClass {
    match write_error(stderr, error, format, pretty) {
        Ok(ErrorWriteStatus::Original) => error.code.exit_class(),
        Ok(ErrorWriteStatus::InternalFallback) | Err(_) => ExitClass::Internal,
    }
}

fn output_failure(_error: std::io::Error) -> AppError {
    AppError::new(
        ErrorCode::Internal,
        "failed to write process output",
        RetrySafety::Safe,
    )
}
