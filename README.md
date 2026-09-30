# fork-release

This branch holds only the automation for this fork. `.github/workflows/fork-release.yml`
runs daily. When `zeronsh/zeron` publishes a release, it applies every branch listed in
`PATCHES`, builds the macOS app, and publishes it as this repo's latest release
(`fork-<version>`).

Current patches:

- `transcript-search` adds full-text transcript search to the command palette.
- `perf/cached-command-discovery` answers `ListSkills` and `ListCommands` from a host
  cache and refreshes it in the background, so the slash menu opens at once.

Those builds set `ZERON_RELEASES_URL` to this repo's latest release, so the in-app updater
installs them. A patch that upstream already contains is skipped. When upstream contains
every patch, the job publishes the stock build, which returns the updater to the official
feed.

Run it by hand from the Actions tab (`fork release`, optionally with `force`).
If it fails, a patch no longer applies to the new release; rebase that branch on upstream
`main`.
