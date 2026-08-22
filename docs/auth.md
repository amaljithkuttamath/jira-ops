# How to authenticate

`jira-ops` supports scoped Jira Cloud API tokens in two modes: a saved login in
the operating system credential store, or a complete set of environment
variables for headless use. Environment credentials take precedence.

OAuth is not supported in this beta.

## Saved login

Use this mode on a workstation with a functioning system credential store.
Create an API token for the Atlassian account that will run the commands, then
pipe the token through standard input:

```bash
read -r -s jira_ops_token
printf '%s\n' "$jira_ops_token" | jira-ops auth login --site https://your-site.atlassian.net --email you@example.com --token-stdin
unset jira_ops_token
```

The site must be an HTTPS `atlassian.net` origin with no path, query, fragment,
port, or embedded credentials. Login resolves the tenant, validates the token by
requesting the current user, stores only identity metadata in the configuration
file, and stores the token in the system credential store.

Check the selected mode without printing the token:

```bash
jira-ops auth status --pretty
```

Verify the credential against Jira:

```bash
jira-ops me --pretty
```

Remove a saved login:

```bash
jira-ops auth logout --pretty
```

`auth logout` does not remove environment variables.

## Headless environment

Set all four variables. A partial set fails with `config_conflict`; it never
falls back to a saved login.

```bash
export JIRA_SITE='https://your-site.atlassian.net'
export JIRA_CLOUD_ID='aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee'
export JIRA_EMAIL='you@example.com'
read -r -s JIRA_API_TOKEN
export JIRA_API_TOKEN
```

Find the cloud ID for the configured site:

```bash
curl -fsS "$JIRA_SITE/_edge/tenant_info"
```

Confirm the tuple:

```bash
jira-ops auth status --pretty
```

Authenticated commands verify that `JIRA_SITE` resolves to `JIRA_CLOUD_ID`
before using the token. The CLI then sends Jira API requests through Atlassian's
cloud gateway.

Clear the environment when finished:

```bash
unset JIRA_SITE JIRA_CLOUD_ID JIRA_EMAIL JIRA_API_TOKEN
```

## Enforce read-only operation

Set the guard in any environment where a write must be impossible:

```bash
export JIRA_READ_ONLY=1
```

With this exact value, every mutation carrying `--apply` fails before credential
resolution or network access. Dry-run planning remains available.

## Token permissions

The token can only perform operations allowed by its account, project
permissions, and scopes. Start with the smallest permissions needed for the
commands you will run. Use `jira-ops schema COMMAND...` to see the stable error
codes for a command. `scope_missing`, `forbidden`, and `auth_invalid` all exit in
the authentication class with code `4`.

Applying `project create` has a stricter official requirement: the account needs
Jira's global **Administer Jira** permission. A scoped token needs either the
classic `manage:jira-configuration` scope or both granular scopes
`write:project:jira` and `read:project:jira`. The local project-creation dry run
does not require credentials or permissions.

## Troubleshooting

- `config_conflict`: remove a partial `JIRA_*` tuple, correct a cloud-ID/site
  mismatch, or unset all Jira environment variables before `auth login`.
- `config_missing`: run `auth login` or provide all four headless variables.
- `keyring_unavailable`: unlock or configure the operating system credential
  store, or use the headless environment mode.
- `auth_invalid`: create a current token and confirm the email belongs to the
  same Atlassian account.
- `scope_missing` or `forbidden`: grant the account and token only the missing
  permission, then retry the read or regenerate the dry-run plan.

Never place a token in a command argument, checked-in file, shell history, issue
body, or diagnostic output. See the [security policy](../SECURITY.md).
