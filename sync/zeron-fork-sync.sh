#!/usr/bin/env bash
# Keeps the fork's main = upstream main + our patch stack.
#
# When upstream main moves, replay the patches (upstream/main..fork/main) onto it,
# signed with your key, check that it builds, and push with a lease on the fork
# main this run started from. Anything that goes wrong leaves the fork untouched
# and raises one notification per distinct failure. Runs hourly from launchd on a
# machine that holds the signing key; GitHub Actions can neither sign as you nor
# push upstream's workflow changes.
set -uo pipefail

SYNC_DIR=${SYNC_DIR:-$HOME/.local/share/zeron-fork-sync}
UPSTREAM_URL=${UPSTREAM_URL:-https://github.com/zeronsh/zeron.git}
FORK_URL=${FORK_URL:-https://github.com/kanishkaverma/heron.git}
GH_REPO=${GH_REPO-kanishkaverma/heron}
CHECK_CMD=${CHECK_CMD:-cargo check -q --locked -p zeron}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$SYNC_DIR/target}
# Fetch git dependencies with the git CLI and its credential helpers. Cargo's built-in
# git follows url.insteadOf rewrites to SSH, and launchd has no SSH agent key.
export CARGO_NET_GIT_FETCH_WITH_CLI=true

repo=$SYNC_DIR/repo
failure_file=$SYNC_DIR/last-failure
mkdir -p "$SYNC_DIR"

log() { printf '%s %s\n' "$(date '+%F %T')" "$*" | tee -a "$SYNC_DIR/sync.log"; }

notify() {
  log "notify: $1"
  if [ -n "${NOTIFY_CMD:-}" ]; then
    eval "$NOTIFY_CMD \"\$1\""
  else
    osascript -e 'on run argv' -e 'display notification (item 1 of argv) with title "Heron sync"' -e 'end run' "$1"
  fi
}

# One notification per (reason, upstream commit): an hourly job must not repeat itself.
fail() {
  local key="$1 $up"
  if [ "$(cat "$failure_file" 2>/dev/null)" != "$key" ]; then
    echo "$key" >"$failure_file"
    notify "$2 Fork main was left as it was. Log: $SYNC_DIR/sync.log"
  else
    log "still failing: $2"
  fi
  exit 1
}

succeed() {
  if [ -f "$failure_file" ]; then
    rm -f "$failure_file"
    notify "Back in sync: fork main is upstream ${up:0:8} plus $(git rev-list --count upstream/main..HEAD) patches."
  fi
  exit 0
}

lock=$SYNC_DIR/lock
if ! mkdir "$lock" 2>/dev/null; then
  if kill -0 "$(cat "$lock/pid" 2>/dev/null)" 2>/dev/null; then
    log "another sync is running (pid $(cat "$lock/pid")); exiting"
    exit 0
  fi
  log "taking over a stale lock"
  rm -rf "$lock" && mkdir "$lock" || exit 1
fi
echo $$ >"$lock/pid"
trap 'rm -rf "$lock"' EXIT

[ -d "$repo/.git" ] || git clone -q "$FORK_URL" "$repo" || { log "clone failed"; exit 2; }
cd "$repo" || exit 1
git remote get-url upstream >/dev/null 2>&1 || git remote add upstream "$UPSTREAM_URL"
git remote set-url upstream "$UPSTREAM_URL"
git remote set-url origin "$FORK_URL"

# Fetch failures are usually just being offline; retry next hour without a notification.
git fetch -q --no-tags upstream '+refs/heads/main:refs/remotes/upstream/main' || { log "fetch upstream failed"; exit 2; }
git fetch -q --no-tags origin '+refs/heads/main:refs/remotes/origin/main' || { log "fetch fork failed"; exit 2; }
up=$(git rev-parse upstream/main)
old=$(git rev-parse origin/main)

git rebase --abort >/dev/null 2>&1
git checkout -q -f --detach "$old" && git clean -fdq

if git merge-base --is-ancestor "$up" "$old"; then
  log "up to date: upstream ${up:0:8}, fork ${old:0:8}"
  succeed
fi

# Probe signing without a pinentry prompt, so a locked key fails fast instead of rebasing unsigned.
gpg_bin=$(git config gpg.program || echo gpg)
signer=$(git config user.signingkey)
echo probe | "$gpg_bin" --batch --pinentry-mode error ${signer:+--local-user "$signer"} --clearsign >/dev/null 2>&1 ||
  fail sign "Can't sign with your GPG key (locked or missing). Sign anything once to unlock it; the next hourly run retries."

log "rebasing $(git rev-list --count "upstream/main..$old") patches from ${old:0:8} onto upstream ${up:0:8}"
if ! git -c commit.gpgsign=true -c rebase.autoStash=false rebase -q upstream/main >>"$SYNC_DIR/sync.log" 2>&1; then
  conflicts=$(git diff --name-only --diff-filter=U | tr '\n' ' ')
  patch=$(git log -1 --format=%s REBASE_HEAD 2>/dev/null)
  git rebase --abort >/dev/null 2>&1
  git checkout -q -f --detach "$old"
  if [ -n "$conflicts" ]; then
    fail conflict "Patch \"$patch\" conflicts with upstream in: $conflicts. Rebase fork main onto upstream main by hand."
  fi
  fail rebase "The rebase onto upstream ${up:0:8} failed. Check the log."
fi

unsigned=$(git log --format='%G? %h %s' upstream/main..HEAD | grep -Ev '^[GU] ')
[ -z "$unsigned" ] || fail sign "Rebased commits came out unsigned: $unsigned"

log "checking the rebased tree: $CHECK_CMD"
bash -c "$CHECK_CMD" >>"$SYNC_DIR/sync.log" 2>&1 ||
  fail check "The patches rebase onto upstream ${up:0:8} but the check fails ($CHECK_CMD)."

git push -q --force-with-lease="main:$old" origin HEAD:refs/heads/main >>"$SYNC_DIR/sync.log" 2>&1 ||
  fail lease "Fork main changed during the sync (a new patch was pushed?). The next run rebases it."
log "pushed fork main $(git rev-parse --short HEAD): upstream ${up:0:8} + $(git rev-list --count upstream/main..HEAD) patches"

# Upstream's workflows (deploy, tests) arrive with its code; keep only ours running in the fork.
if [ -n "$GH_REPO" ]; then
  gh workflow list -R "$GH_REPO" --json id,path,state \
    --jq '.[] | select(.state == "active" and .path != ".github/workflows/fork-release.yml") | .id' |
    while read -r id; do gh workflow disable "$id" -R "$GH_REPO" && log "disabled upstream workflow $id in the fork"; done
  # It rebuilds only if upstream released or the patch stack changed.
  gh workflow run fork-release.yml -R "$GH_REPO" --ref fork-release && log "dispatched fork release"
fi

succeed
