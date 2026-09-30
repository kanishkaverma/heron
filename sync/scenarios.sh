#!/usr/bin/env bash
# E2E scenarios for zeron-fork-sync against throwaway bare repos (upstream + fork).
# Each line of output is PASS/FAIL <scenario>; the transcript is the artifact.
#
# How the sync can fail, and what it must do instead:
#  1 upstream has nothing new            -> no push, fork main untouched, exit 0
#  2 upstream moved, stack rebases clean -> fork main = upstream + same patches, all signed, pushed
#  3 a patch conflicts with upstream      -> no push, notify naming the conflicting file, repo left clean
#  4 rebase clean but the check fails     -> no push, notify "check failed"
#  5 GPG cannot sign (key locked)         -> no push, nothing unsigned anywhere, notify "sign"
#  6 fork main moved during the run       -> lease rejects, the concurrent patch survives, notify
#  7 upstream merged one patch verbatim   -> that patch drops out, the rest stay on top
#  8 every patch upstreamed               -> fork main == upstream main
#  9 a second run while one is running    -> second exits without touching anything
# 10 the same failure on every hourly run -> one notification, not one per run
# 11 recovery after a failure            -> one "back in sync" notification
# 12 fetch fails (offline)               -> no push, non-zero exit, no notification
set -uo pipefail
SYNC=${SYNC:-$(dirname "$0")/zeron-fork-sync.sh}
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
export GNUPGHOME=$T/gnupg; mkdir -m700 "$GNUPGHOME"
gpg -q --batch --passphrase '' --quick-gen-key 'sync test <t@example.com>' ed25519 sign never 2>/dev/null
KEY=$(gpg --list-keys --with-colons | awk -F: '/^fpr/{print $10; exit}')
export GIT_CONFIG_GLOBAL=$T/gitconfig
git config --global user.name tester; git config --global user.email t@example.com
git config --global user.signingkey "$KEY"; git config --global init.defaultBranch main
git config --global advice.detachedHead false

pass=0; fail=0
check() { if eval "$2"; then echo "PASS  $1"; pass=$((pass+1)); else echo "FAIL  $1   ($2)"; fail=$((fail+1)); fi; }

setup() { # fresh upstream with a base commit, fork = upstream + two signed patches
  rm -rf "$T/up.git" "$T/fork.git" "$T/w" "$T/sync" "$T/notes"; : >"$T/notes"
  git init -q --bare "$T/up.git"; git init -q --bare "$T/fork.git"
  git clone -q "$T/up.git" "$T/w" 2>/dev/null; cd "$T/w"
  printf 'a\nb\nc\n' >core.txt; git add .; git commit -qm base; git push -q origin main
  git remote add fork "$T/fork.git"
  echo p1 >patch1.txt; git add .; git -c commit.gpgsign=true commit -qm "patch one"
  printf 'a\nb\nPATCHED\n' >core.txt; git -c commit.gpgsign=true commit -qam "patch two"
  git push -q fork HEAD:main; git reset -q --hard origin/main; cd - >/dev/null
}
upstream_commit() { # $1 file $2 content $3 msg
  cd "$T/w" && git fetch -q origin && git reset -q --hard origin/main && printf "$2" >"$1" && git add . && git commit -qm "$3" && git push -q origin main; cd - >/dev/null
}
run() { SYNC_DIR=$T/sync UPSTREAM_URL=$T/up.git FORK_URL=$T/fork.git GH_REPO= \
        NOTIFY_CMD="echo >>$T/notes" CHECK_CMD="${CHECK_CMD:-true}" bash "$SYNC" >"$T/out" 2>&1; }
fork_main() { git --git-dir="$T/fork.git" rev-parse main; }
up_main() { git --git-dir="$T/up.git" rev-parse main; }
stack() { git --git-dir="$T/fork.git" log --format=%s "$(up_main)..main" | tr '\n' ,; }
unsigned_on_fork() { git --git-dir="$T/fork.git" log --format=%G? "$(up_main)..main" | grep -vc G; }
notes() { wc -l <"$T/notes" | tr -d ' '; }

# 1
setup; before=$(fork_main); run; rc=$?
check "1 nothing new: exit 0" "[ $rc = 0 ]"
check "1 nothing new: fork untouched" "[ $(fork_main) = $before ]"
check "1 nothing new: no notification" "[ $(notes) = 0 ]"

# 2
setup; upstream_commit other.txt 'x\n' "upstream work"; run; rc=$?
check "2 clean: exit 0" "[ $rc = 0 ]"
check "2 clean: upstream main is an ancestor of fork main" "git --git-dir=$T/fork.git merge-base --is-ancestor $(up_main) main"
check "2 clean: stack preserved in order" "[ '$(stack)' = 'patch two,patch one,' ]"
check "2 clean: every patch signed" "[ $(unsigned_on_fork) = 0 ]"

# 3
setup; upstream_commit core.txt 'a\nb\nUPSTREAM\n' "upstream edits core"; before=$(fork_main); run; rc=$?
check "3 conflict: non-zero exit" "[ $rc != 0 ]"
check "3 conflict: fork untouched" "[ $(fork_main) = $before ]"
check "3 conflict: notification names core.txt" "grep -q core.txt $T/notes"
check "3 conflict: sync repo not mid-rebase" "[ ! -d $T/sync/repo/.git/rebase-merge ] && [ ! -d $T/sync/repo/.git/rebase-apply ]"
# 10 (same failure again)
run
check "10 repeat failure: still one notification" "[ $(notes) = 1 ]"
# 11 (resolve upstream side, sync recovers)
upstream_commit core.txt 'a\nb\nc\n' "upstream reverts core"; run; rc=$?
check "11 recovery: exit 0" "[ $rc = 0 ]"
check "11 recovery: one 'back in sync' notification" "[ $(notes) = 2 ] && tail -1 $T/notes | grep -qi 'back in sync'"

# 4
setup; upstream_commit other.txt 'x\n' "upstream work"; before=$(fork_main); CHECK_CMD=false run; rc=$?
check "4 check fails: non-zero exit" "[ $rc != 0 ]"
check "4 check fails: fork untouched" "[ $(fork_main) = $before ]"
check "4 check fails: notification says check" "grep -qi check $T/notes"

# 5
setup; upstream_commit other.txt 'x\n' "upstream work"; before=$(fork_main)
git config --global gpg.program false; run; rc=$?; git config --global --unset gpg.program
check "5 no gpg: non-zero exit" "[ $rc != 0 ]"
check "5 no gpg: fork untouched" "[ $(fork_main) = $before ]"
check "5 no gpg: notification mentions signing" "grep -qi sign $T/notes"

# 6
setup; upstream_commit other.txt 'x\n' "upstream work"
CHECK_CMD="cd $T/w && git fetch -q fork && git checkout -q fork/main && echo p3 >p3.txt && git add . && git -c commit.gpgsign=true commit -qm 'patch three' && git push -q fork HEAD:main && git checkout -q main" run; rc=$?
check "6 lease: non-zero exit" "[ $rc != 0 ]"
check "6 lease: concurrent patch survives" "git --git-dir=$T/fork.git log --format=%s -1 main | grep -q 'patch three'"
check "6 lease: notified" "[ $(notes) = 1 ]"

# 7
setup; cd "$T/w"; echo p1 >patch1.txt; git add .; git commit -qm "upstream adopts patch one"; git push -q origin main; cd - >/dev/null; run; rc=$?
check "7 adopted: exit 0" "[ $rc = 0 ]"
check "7 adopted: only patch two remains" "[ '$(stack)' = 'patch two,' ]"

# 8
setup; cd "$T/w"; echo p1 >patch1.txt; printf 'a\nb\nPATCHED\n' >core.txt; git add .; git commit -qm "upstream adopts both"; git push -q origin main; cd - >/dev/null; run; rc=$?
check "8 all adopted: exit 0" "[ $rc = 0 ]"
check "8 all adopted: fork main == upstream main" "[ $(fork_main) = $(up_main) ]"

# 9
setup; upstream_commit other.txt 'x\n' "upstream work"; before=$(fork_main)
mkdir -p "$T/sync"; sleep 30 & holder=$!; mkdir "$T/sync/lock"; echo $holder >"$T/sync/lock/pid"; run; rc=$?
check "9 overlap: exit 0" "[ $rc = 0 ]"
check "9 overlap: fork untouched" "[ $(fork_main) = $before ]"
kill $holder; wait $holder 2>/dev/null
run; check "9 stale lock (dead pid) is taken over" "[ $(fork_main) != $before ]"

# 12
setup; before=$(fork_main)
SYNC_DIR=$T/sync UPSTREAM_URL=$T/nope.git FORK_URL=$T/fork.git GH_REPO= NOTIFY_CMD="echo >>$T/notes" CHECK_CMD=true bash "$SYNC" >"$T/out" 2>&1; rc=$?
check "12 offline: non-zero exit" "[ $rc != 0 ]"
check "12 offline: fork untouched" "[ $(fork_main) = $before ]"
check "12 offline: no notification" "[ $(notes) = 0 ]"

echo "$pass passed, $fail failed"
[ $fail = 0 ]
