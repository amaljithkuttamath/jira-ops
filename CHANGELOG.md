# Changelog

All notable changes to `jira-ops` are recorded here.

## [Unreleased]

## [0.2.0-beta.2] - 2026-08-22

### Fixed

- Release publication now identifies the repository explicitly when the final
  job runs without a source checkout.

### Security

- Added weekly Dependabot updates for Cargo and GitHub Actions.
- Added CodeQL scanning for Rust and workflow code on pushes, pull requests,
  and a weekly schedule.
- Enabled GitHub dependency alerts, automatic security updates, secret
  scanning, and push protection for the public repository.

## [0.2.0-beta.1] - 2026-08-20

### Added

- Functional Jira Cloud parity for configuration, discovery, boards, releases,
  cloning and deletion, remote links, worklogs, epics, and sprints.
- Deterministic text, Markdown, and ADF content inputs; shell completions, man
  pages, and canonical URL output.
- Exact target-bound confirmations for destructive issue, link, worklog, epic,
  and sprint operations.
- Release gates for dependency advisories and licenses, 80% line coverage,
  source-history secret scanning, and packaged-archive secret scanning.

### Changed

- The command surface remains non-interactive and agent-first. Discovery plus
  stable IDs replaces a TUI; JSON/TOON replaces CSV/raw output; URL commands do
  not launch a browser.

### Beta limits

- Jira Cloud and API-token authentication only. OAuth and self-managed Jira are
  not supported.

## [0.1.0-beta.1] - 2026-08-20

### Added

- Jira Cloud API-token authentication through the system credential store or a
  complete headless environment tuple.
- Structured JSON output and optional TOON output for success and error
  documents.
- Offline schema discovery for the complete command contract.
- Project listing, lookup, local template discovery, and guarded project
  creation.
- Field discovery and JQL issue search with opaque, query-bound cursors.
- Issue lookup, create-metadata discovery, creation, update, assignment,
  comments, and transitions.
- Issue-link type discovery, link projection, guarded link addition, watcher
  listing, and guarded watcher addition and removal.
- Dry-run plans by default and explicit apply intent for every Jira mutation.
- Stable exit classes, mutation outcomes, retry-safety metadata, rate-limit
  metadata, response limits, and redirect rejection.
- Release archives for macOS, Linux, and Windows with SHA-256 checksums.

### Beta limits

- Jira Cloud only.
- API tokens only; OAuth is not implemented.
- No issue or project deletion, issue-link deletion, attachments, worklogs,
  boards, sprints, filters, or bulk mutation commands.
- Release interfaces may change before `1.0.0`; use `contract_version` to detect
  machine-contract changes.

[Unreleased]: https://github.com/amaljithkuttamath/jira-ops/compare/v0.2.0-beta.2...HEAD
[0.2.0-beta.2]: https://github.com/amaljithkuttamath/jira-ops/releases/tag/v0.2.0-beta.2
[0.2.0-beta.1]: https://github.com/amaljithkuttamath/jira-ops/releases/tag/v0.2.0-beta.1
[0.1.0-beta.1]: https://github.com/amaljithkuttamath/jira-ops/releases/tag/v0.1.0-beta.1
