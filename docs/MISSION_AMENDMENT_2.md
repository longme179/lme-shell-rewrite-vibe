# Amendment 2: GitHub backup and recovery

Read together with docs/MISSION_CONTINUATION.md. Where they conflict, this one
wins. It replaces items 1 and 3 of that prompt's Session setup, and adds the
sections below plus two lines in Delivery.

> Session-setup block (credentials, shell exports) intentionally NOT stored
> here, per the "never write this block into any file" rule. It lives only in
> the chat messages. Recovered sessions: see STATE.md "Environment".

# Backup repo rules

https://github.com/longme179/lme-shell-rewrite-vibe.git is PUBLIC. Assume
everything pushed there is public. Its `main` holds the owner's own files
(README.md, lme-shell/, ckpt-009/).
- Push only `rewrite/*` branches and the `wip/<branch>` snapshot ref, and only
  through scripts/gh-sync.sh. Never push main. Never force-push except the wip/
  snapshot ref. Never delete anything except the selftest ref.
- If a push is rejected as non-fast-forward, do not force it. Stop pushing, note
  it in STATE.md, and keep working with Discord checkpoints.
- The webhook URL and the token must never appear in pushed content. The secret
  scan enforces this and aborts with exit 3. If it aborts, find the leak, remove
  it from local history BEFORE any push, and tell me.
- Run scripts/gh-sync.sh from scripts/checkpoint.sh: insert
  `scripts/gh-sync.sh || echo "WARNING: gh-sync failed (exit $?); see STATE.md" >&2`
  right after its secret-scan block. Also run it after every commit that closes
  a feature.

# Recovery (first thing in every session; stop at the first path that works)

Invariant: in the working repo, remote `origin` is ALWAYS the upstream repo
https://github.com/longme179/lme-fedora-niri-qs.git (checkpoint.sh and deliver.sh
rely on origin/main). The backup repo is never a configured remote.
a. ~/lme-fedora-niri-qs exists: continue from STATE.md.
b. Backup repo has a branch rewrite/native (check with
   `git ls-remote --heads https://github.com/longme179/lme-shell-rewrite-vibe.git`):
     git init ~/lme-fedora-niri-qs && cd ~/lme-fedora-niri-qs
     git remote add origin https://github.com/longme179/lme-fedora-niri-qs.git && git fetch origin
     git fetch https://github.com/longme179/lme-shell-rewrite-vibe.git \
       'refs/heads/rewrite/*:refs/heads/rewrite/*' 'refs/heads/wip/*:refs/remotes/vibe-wip/*'
     git checkout rewrite/native
   If refs/remotes/vibe-wip/rewrite/native exists and its parent is the tip of
   rewrite/native, restore the uncommitted work:
     git restore --source=refs/remotes/vibe-wip/rewrite/native --staged --worktree .
   then commit it as "restore wip snapshot".
c. Else clone the backup repo's main into a scratch dir and look for
   ckpt-*/repo.bundle. Use the highest-numbered one: reassemble parts if split,
   `git clone` it per that checkpoint's RESTORE.txt, run
   `git remote set-url origin <upstream url above> && git fetch origin`, checkout
   rewrite/native, apply wip.patch and untracked.tar if present.
d. Else, if the backup main has lme-shell/ (a source export without history):
   clone upstream, read the base commit in lme-shell/STATE.md (default ef5281c),
   `git checkout -b rewrite/native <base>`. Every path that exists in the upstream
   tree at that commit and is absent from lme-shell/, except docs/previews and
   assets/wallpapers, is `git mv`'d into legacy/<same path>. Then copy the
   lme-shell/ contents over the tree and commit "restore from source export".
   This is best effort: list the moves in docs/DECISIONS.md.
e. Else write only: "RESET: nothing recoverable found" and stop.
After recovery (b, c or d): run scripts/gh-sync.sh so the backup repo gets a
proper rewrite/native branch. Then check upstream: it moved after your TIER-1
(you saw main @ 56fc7a4). Run `git fetch origin` and
`git diff --stat <base>..origin/main -- qml scripts docs README.md`. If features
you still have to build changed, append a section to docs/UPSTREAM_SYNC.md and
update NATIVE_PLAN.md. Do not rebase rewrite/native; read new upstream behavior
with `git show origin/main:<path>`. Then continue the continuation prompt at its
current step.

# Delivery additions

- Every summary message and the README say where the source also lives: backup
  repo URL, branch rewrite/native, and the pushed commit hash.
- FINAL: run scripts/gh-sync.sh once more after the delivery and report the hash.

# scripts/gh-sync.sh (create exactly this, chmod +x)

The canonical copy of this script is scripts/gh-sync.sh in the repo, created
verbatim from this section (see git history of this file for the exact text;
it is reproduced here unchanged).

Key properties (full text in scripts/gh-sync.sh):
- Never prints the token; reads ~/.lme-gh-token; GIT_ASKPASS helper at
  ~/.lme-askpass.sh (created on demand, mode 700).
- `--selftest`: pushes then deletes a selftest/<timestamp> ref on the backup
  repo; prints "gh-sync selftest OK".
- Normal run: builds a wip snapshot commit (tracked+untracked, .gitignore
  honored) on a side ref, secret-scans the snapshot and ALL history
  (fail-closed, exit 3), pushes refs/heads/rewrite/* and +wip/<current branch>.
