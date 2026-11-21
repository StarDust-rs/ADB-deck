# Releasing

A release is made from `main` in the GitHub web interface. The workflow in
`.github/workflows/release.yml` does the rest.

## Making a release

1. Merge what belongs in the release into `main`.
2. On GitHub: **Releases → Draft a new release**.
3. **Choose a tag**: type the new version, for example `v1.0.0`, and pick **Create new tag on publish**.
   Leave the target on `main`. The tag must be `vMAJOR.MINOR.PATCH`.
4. Leave the notes empty (or write an introduction; see below) and press **Publish release**.

Publishing starts the workflow. When it finishes, the release has the RPMs, `SHA256SUMS`
and the notes. A failed run leaves the release without files; fix the cause and use
**Actions → Release → Run workflow** with the same tag.

## What the workflow does

| Job | What it does |
|---|---|
| `prepare` | Refuses a tag that is not `vX.Y.Z` or whose commit is not on `main`. Writes the release notes. Takes the commit time for reproducible builds. |
| `build` | In the Fedora 45 container (x86_64): stamps the version from the tag into `Cargo.toml`, `Cargo.lock` and the AppStream release entry (the build tree only, nothing is committed), runs `packaging/build-rpm.sh` (vendors the crates, builds offline, runs the test suite and the desktop file and AppStream validation in the spec's `%check`), installs the RPM with `dnf`, checks `adbdeck --version` prints the tag's version and the shipped files, and runs `rpmlint` for information. |
| `publish` | Attaches the RPM, the source RPM and `SHA256SUMS` to the release, adds a build provenance attestation (public repositories) and writes the notes. Nothing is published unless the build succeeded. |

The version number is typed once, in the tag. The `version` in `Cargo.toml` on `main`
is not the release version and does not need to be changed.

## Release notes

The notes list the titles of the commits between the previous `v*` tag and the new one,
newest first, with the short hash. Commits titled `feat:` or `fix:` (optionally with a scope,
`fix(adb):`) are grouped under *Features* and *Fixes*; every other title is listed under
*Other changes*. Merge commits are skipped, the commits they bring are listed. For the first
release every commit is listed. A *Full changelog* compare link ends the notes.

Write good commit titles, or squash-merge pull requests with a good title: that is the
release note. To preview the notes locally, from a clone with the tags:

    packaging/release-notes.sh v1.0.0

Text typed into the release description before publishing is kept above the generated
notes. The generated part is replaced when the workflow runs again.

## Changing the target

The workflow builds one variant: Fedora 45, x86_64. To change it, edit the container image (`fedora:45`),
the `.fc45` dist tag in the *Build the RPMs* step and the `fc45.x86_64` names in the later steps.
Fedora 43 ends on 2026-12-09. The debuginfo and debugsource RPMs are built but not published.
Another branch than `main`: `MAIN_BRANCH` at the top of the workflow.

## Known limits

* The RPMs are not signed (see `docs/PACKAGING_REPORT.md`); verify a download with `SHA256SUMS` or
  `gh attestation verify FILE --repo OWNER/REPO`.
* Only stable tags, `vMAJOR.MINOR.PATCH`. RPM versions cannot contain `-`, so `v1.0.0-rc1` is refused.
* The RPM dependency names were written without a Fedora repository to check them. The
  install step in `build` is the first real check; if it fails, the log names the package.
* Pushing a tag from the command line, without creating a release, does not start the workflow.
