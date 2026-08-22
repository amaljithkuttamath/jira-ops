# Ops CLI for Jira

Ops CLI for Jira is a predictable command-line client for Jira Cloud, installed
as `jira-ops`. It emits one machine-readable document per invocation, exposes
its own command schemas, and plans every Jira mutation before it writes.

> `jira-ops` is an independent, third-party project. It is not affiliated with,
> endorsed by, or sponsored by Atlassian. Jira and Atlassian are trademarks of
> Atlassian Pty Ltd.

This release is a public beta. It supports Jira Cloud and API-token
authentication. OAuth and self-managed Jira are not supported.

## Install

Download the archive for your platform from
[GitHub Releases](https://github.com/amaljithkuttamath/jira-ops/releases). Each
archive has a matching `.sha256` file.

Verify an archive on Linux:

```bash
sha256sum -c jira-ops-v0.2.0-beta.1-x86_64-unknown-linux-gnu.tar.gz.sha256
```

Verify an archive on macOS:

```bash
shasum -a 256 -c jira-ops-v0.2.0-beta.1-aarch64-apple-darwin.tar.gz.sha256
```

Checksums detect a corrupted download. Also verify the GitHub artifact
provenance attestation, which binds the downloaded archive to this repository's
release workflow:

```bash
gh attestation verify jira-ops-v0.2.0-beta.1-x86_64-unknown-linux-gnu.tar.gz \
  --repo amaljithkuttamath/jira-ops
```

You can also build from source with Rust 1.90 or newer:

```bash
cargo install --locked --git https://github.com/amaljithkuttamath/jira-ops --tag v0.2.0-beta.1
```

Confirm the installation:

```bash
jira-ops version
```

## First use

Inspect the complete command contract without credentials:

```bash
jira-ops schema --all --pretty
```

List the built-in project templates without contacting Jira:

```bash
jira-ops project templates --type software --pretty
```

Configure an API token using the system credential store:

```bash
read -r -s jira_ops_token
printf '%s\n' "$jira_ops_token" | jira-ops auth login --site https://your-site.atlassian.net --email you@example.com --token-stdin
unset jira_ops_token
```

Verify the active identity:

```bash
jira-ops me --pretty
```

See [authentication](docs/auth.md) for headless environment variables.

## Safe writes

Mutation input is one JSON object on standard input. The exact `--input -`
marker is required. Without `--apply`, a mutation returns a plan and does not
send the write request.

This project creation example is completely local:

```bash
printf '%s\n' '{"key":"DEMO","name":"Demo project","project_type_key":"software","project_template_key":"com.pyxis.greenhopper.jira:gh-simplified-kanban-classic","lead_account_id":"replace-with-account-id"}' | jira-ops project create --input - --pretty
```

Inspect the plan. To write, add `--apply` to the same command. Set
`JIRA_READ_ONLY=1` in automation that must never apply a mutation.

Applying project creation requires the account to have Jira's global
**Administer Jira** permission. A scoped token needs either the classic
`manage:jira-configuration` scope or both granular scopes
`write:project:jira` and `read:project:jira`.

## Output contract

- Success is written to standard output as `{"data": ...}` and exits `0`.
- Failure is written to standard error as `{"error": ...}` and exits nonzero.
- JSON is compact by default. Add `--pretty` for indented JSON.
- Add `-o toon` or `--output toon` for token-oriented TOON output.
- `--pretty` and TOON are mutually exclusive.
- `--timeout-ms` accepts `1000` through `120000`; the default is `30000`.
- Paginated commands return `meta.next_cursor`. Cursors are opaque and bound to
  the exact command and query.

The CLI's own schema is the normative reference:

```bash
jira-ops schema issue create --pretty
```

The interface is deliberately non-interactive: there is no TUI, prompt, or
browser launch. Interactive selection is replaced by discovery commands plus
stable IDs; `url` returns a URL instead of opening it. JSON and
TOON replace CSV/raw output so agents receive one typed document on one stream.

## Commands

| Command | Purpose |
| --- | --- |
| `version` | Show CLI and contract versions |
| `schema [COMMAND...]` | Discover all commands or one command contract |
| `config get`, `config set`, `config unset` | Read or update saved non-secret defaults |
| `url issue`, `url project` | Return canonical browse URLs without opening a browser |
| `completion` | Generate shell completion text |
| `man` | Generate man pages into a validated empty directory |
| `server info` | Read stable Jira Cloud server metadata |
| `user search` | Search users with privacy-trimmed output |
| `board list` | List Jira Software boards |
| `release list` | List project releases |
| `auth login`, `auth status`, `auth logout` | Manage API-token authentication |
| `me` | Show the authenticated Jira user |
| `project list`, `project get` | Read projects |
| `project templates`, `project create` | Discover local templates and plan or create a project |
| `field list` | List or search fields |
| `issue get`, `issue search` | Read issues |
| `issue create-meta` | Discover issue types and field metadata |
| `issue create`, `issue clone`, `issue update`, `issue delete` | Plan, apply, or explicitly confirm issue changes |
| `issue assign` | Plan or apply assignment or unassignment |
| `issue link types`, `issue link get`, `issue link add`, `issue link remove` | Discover, inspect, add, or explicitly remove issue links |
| `issue remote-link list`, `issue remote-link get`, `issue remote-link add`, `issue remote-link remove` | Read or safely change HTTPS remote links |
| `issue worklog list`, `issue worklog add`, `issue worklog update`, `issue worklog delete` | Read or safely change worklogs and estimates |
| `epic list`, `epic create`, `epic add`, `epic remove` | Discover, create, or safely change epic membership |
| `sprint list`, `sprint issues`, `sprint add`, `sprint close` | Discover sprints, move issues, or safely close an active sprint |
| `issue watcher list`, `issue watcher add`, `issue watcher remove` | Read or change watchers |
| `issue comments`, `issue comment` | List or add comments |
| `issue transitions`, `issue transition` | Discover or apply workflow transitions |

## Documentation

- [Authentication](docs/auth.md): keyring and headless setup
- [Recipes](docs/recipes.md): projects, issues, comments, transitions, and pagination
- [Machine-use guide](docs/agent-guide.md): schemas, retries, cursors, and compact output
- [Security policy](SECURITY.md): supported versions and private reporting
- [Changelog](CHANGELOG.md): release contents and beta limits

## Release process

The tag-triggered GitHub Actions workflow is the authoritative release path.
A maintainer prepares a reviewed, clean commit on `main`, confirms that the
`v<version>` tag matches `Cargo.toml`, then creates and pushes that tag. The
workflow checks out that exact tag commit and requires formatting, linting,
tests, a release build, dependency-policy checks, line coverage of at least
80%, and a full-history secret scan before it builds, attests, scans, and
publishes archives. Do not publish with `gh release create` or `cargo release`.

`release.toml` deliberately limits cargo-release preparation to `main` and
disables its push and publish operations; GitHub Actions performs publishing
only after the tag gate succeeds.

## License

Licensed under either [MIT](LICENSE-MIT) or
[Apache License 2.0](LICENSE-APACHE), at your option.
