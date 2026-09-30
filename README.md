# fork-release

Automation for this fork of `zeronsh/zeron`. The fork is two things:

- **`main`** is upstream `main` with our patches on top. That stack of commits is the fork.
  An hourly job keeps it rebased on upstream, so `main` always has both.
- **Releases** (`fork-<version>`) are each upstream release rebuilt with that stack. The
  in-app updater follows them.

## Adding, changing or dropping a patch

Work on top of the fork's `main` and push to it:

    git fetch fork && git switch -C fork-main fork/main
    # commit (signed) …
    git push fork HEAD:main

To change or drop a patch, `git rebase -i` the stack and push with
`--force-with-lease`. A patch that is also an upstream PR can keep its own branch for the
PR; `main` is what ships.

Because the sync rebases `main`, a local copy of it goes stale when upstream moves:
`git fetch fork && git reset --hard fork/main`.

## Keeping main current: `sync/`

`sync/zeron-fork-sync.sh` runs hourly from launchd on a Mac that has the GPG signing key
and a `gh` login. GitHub Actions can't do this job: it can't sign as you, and its token
can't push upstream's frequent workflow changes. When upstream `main` moves, the job:

1. replays `upstream/main..main` onto it, signed; patches upstream already has drop out;
2. runs `cargo check -p zeron`;
3. pushes with a lease on the `main` it started from;
4. disables any upstream workflow in the fork (deploy, tests) and dispatches `fork release`.

If anything fails (conflict, check, locked GPG key, `main` pushed mid-run), `main` is
left as it was and you get one macOS notification per failure. The job keeps retrying
hourly and sends "back in sync" when it recovers. To fix a conflict, rebase by hand
(`git rebase origin/main` from `fork-main`, then `git push --force-with-lease fork HEAD:main`).

    sync/install.sh                             # install or update the LaunchAgent
    ~/.local/share/zeron-fork-sync/sync.log     # what it did
    sync/scenarios.sh                           # E2E failure scenarios against throwaway repos

## Releases: `.github/workflows/fork-release.yml`

Runs daily and after every sync push. It takes upstream's latest release tag, replays
`main`'s patch stack onto it, builds the macOS app, and publishes it as this repo's
latest release. It rebuilds when upstream releases or when the stack's content changes;
the stack fingerprint is in each release's notes.

Those builds set `ZERON_RELEASES_URL` to this repo's latest release, so the in-app updater
installs them. When upstream contains every patch, the job publishes the stock build,
which returns the updater to the official feed.

A new patch on the same upstream version republishes the same version number, which the
updater doesn't treat as an update. Install that build by hand, or wait for the next
upstream release.
