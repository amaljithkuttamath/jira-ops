# Releasing jira-ops

The tag-triggered GitHub Actions workflow is the only supported publication
path.

1. Merge the release change through a pull request after every required check
   passes on the current `main` branch.
2. Confirm that `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, `README.md`, and
   `SECURITY.md` name the same version.
3. Create an annotated `v<version>` tag on the resulting `main` commit and push
   the tag.
4. Wait for the release workflow to validate the tag, rerun the complete quality
   gate, build all supported platforms, verify checksums, scan unpacked
   archives, create provenance attestations, and publish the GitHub release.
5. Wait for the
   [`homebrew-tap`](https://github.com/amaljithkuttamath/homebrew-tap)
   updater to verify the four Unix archives, open its formula pull request, pass
   macOS and Linux installation checks, and merge. The updater runs hourly and
   can also be dispatched manually with the exact release tag.
6. Download one published archive and verify its checksum, provenance, and
   reported CLI version.

The tag must exactly match the package version. The release workflow checks out
that immutable commit and will not build or publish from a different revision.

Do not publish with `gh release create` or `cargo release`. `release.toml`
limits cargo-release to local preparation and disables its push and publish
operations.
