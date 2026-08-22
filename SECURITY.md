# Security policy

## Supported versions

| Version | Supported |
| --- | --- |
| `0.2.0-beta.2` | Yes |
| `0.2.0-beta.1` | No |
| `0.1.0-beta.1` | No |
| Older builds | No |

This beta supports Jira Cloud with API-token authentication. It does not support
OAuth or self-managed Jira.

## Report a vulnerability

If private vulnerability reporting is available in the repository's
**Security** tab, choose **Report a vulnerability** to submit a private report.
Do not open a public issue for an unpatched vulnerability.

Include the affected version, operating system, command family, reproduction
steps, impact, and whether a Jira write may have occurred. Remove API tokens,
email addresses, cloud IDs, site names, issue content, and authorization headers
from the report.

If private reporting is unavailable, open an issue that says only that you need
a private security contact. Do not post exploit details or credentials.

## Credential handling

- `auth login` reads one token from standard input. It does not accept a token
  argument.
- Saved tokens are stored in the operating system credential store. The local
  configuration file contains the site and account identity, not the token.
- Headless credentials require `JIRA_SITE`, `JIRA_CLOUD_ID`, `JIRA_EMAIL`, and
  `JIRA_API_TOKEN` together. A partial tuple fails closed.
- Tokens are used only for HTTPS requests through Atlassian's cloud gateway.
- Redirects are rejected, so credentials are not replayed to a redirect target.
- Response bodies and error prose are not copied into stable authentication
  errors.

Treat command output as potentially sensitive Jira data. Restrict logs and build
artifacts even though the CLI redacts credentials from its own envelopes.

## Mutation safety

Jira mutations are plans unless explicit apply intent is present. Set
`JIRA_READ_ONLY=1` to reject all applied mutations before credential or network
access.

The CLI sends each write request at most once. On failure, use
`operation_outcome` and `retry_safety` from the error document:

- `not_applied` and `safe`: correct the cause or regenerate the plan
- `applied` and `unsafe`: verify the resource; do not replay the request
- `unknown` and `unknown`: stop automation and reconcile with Jira

## Release verification

Release archives include detached SHA-256 checksum files. Verify the checksum
before installing a binary, then verify the GitHub artifact provenance
attestation for the archive:

```bash
gh attestation verify jira-ops-v0.2.0-beta.2-x86_64-unknown-linux-gnu.tar.gz \
  --repo amaljithkuttamath/jira-ops
```

Checksums detect corruption; the attestation verifies that GitHub built the
artifact from this repository's release workflow. GitHub Actions used by that
workflow are pinned to immutable commit hashes.

## Automated security checks

- `cargo deny check` blocks known RustSec advisories, yanked crates,
  disallowed licenses, and unapproved dependency sources in CI and releases.
- Dependabot monitors `Cargo.lock` and opens security updates for vulnerable
  Rust dependencies.
- CodeQL scans Rust source and GitHub Actions workflows on `main`, pull
  requests, and a weekly schedule.
- Secret scanning and push protection reject supported credential patterns.

`jira-ops` is an independent, third-party project. It is not affiliated with,
endorsed by, or sponsored by Atlassian.
