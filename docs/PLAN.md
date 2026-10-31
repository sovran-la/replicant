# Replicant — Rough Plan

*Draft 2026-10-31, rev 2 (home-base model, single `sync`, conflict UX).
Captures the design discussion; nothing here is final.*

## Problem

`~/work` is kept identical across machines with Syncthing, including every
repo's `.git`. That has failed repeatedly (Aug 28, Sep 5, Oct 27 ×3):

1. `.git/index` is per-machine data (it caches stat info). Syncing it makes
   git on the receiver see a cache describing another machine's filesystem —
   phantom staged deletions, status lies.
2. Syncthing syncs files independently, so a ref can land before the objects
   it points at. Syncthing reports "up to date" while `git fsck` finds missing
   objects.
3. Two machines active on the same repo (e.g. an agent on twinoaks while
   editing on birchbook) has no merge, only last-write-wins.

A "pause Syncthing during git ops" lock fixes torn writes only; it does not
touch 1–3. Forking Syncthing, or a hosted alternative like rbox, keeps the
continuous-bidirectional model that is the actual root cause.

## Requirements

- Every repo exists on every machine that is allowed to have it, in the
  exact same state: branches, HEAD, index, uncommitted working tree, stashes.
- Some machines are restricted. twinoaks only ever holds `tundra/` and
  `sequoia/`; nothing else may land there.
- Not everything under `~/work` is git-appropriate (datasets, scratch,
  secrets). Those move too.
- macOS and Linux only. Home differs: `/Users/morgan` vs `/home/morgan`.
- No Tokio. `std::thread` + channels.

## Model: home base

**thistledown is home.** It is always on, headless, holds the unrestricted
tree, and is already the Syncthing hub and the de facto backup of `~/work`.
Nobody works on it directly; it only ever changes via `sync`.

Every other machine is a leaf. A leaf talks only to home, never to another
leaf. Transport is `rsync -a --delete` over Tailscale SSH. Replicant never
copies bytes itself; it is policy and guardrails around rsync.

### One command

```
replicant sync              # figure out the direction, show it, do it
replicant sync --yes        # don't wait for confirmation
replicant sync --dry-run    # decision + itemized rsync diff, no writes
replicant status            # the decision table without acting; unresolved conflicts in red
```

Direction is inferred, never typed. Each leaf keeps a **baseline**: the
state it and home last agreed on. Home keeps one baseline per leaf.

```
home same,    leaf same     → nothing to do
home changed, leaf same     → pull   (make leaf match home)
home same,    leaf changed  → push   (make home match leaf)
both changed                → conflict resolution, see below
```

`sync` always prints what it decided before touching anything:

```
push → thistledown: 3 repos, 41 files changed, 0 deleted
```

State is a hash over per-repo `(HEAD, dirty-tree hash)` plus a manifest of
non-repo files, scoped to the leaf's prefixes.

### Config lives on home

Home holds the fleet config: hosts, each host's prefixes, excludes. A leaf's
local config is one line — where home is. Everything else comes down on
first contact. Adding or restricting a host is an edit on thistledown, and
home enforces it: it serves and accepts only what a host's prefixes allow.

```toml
# thistledown: ~/.config/replicant/fleet.toml
root    = "/home/morgan/work"
exclude = ["target", ".build", "node_modules", "bin", ".DS_Store"]

[hosts.birchbook]
root     = "/Users/morgan/work"
prefixes = ["*"]

[hosts.twinoaks]
root     = "/Users/morgan/work"
prefixes = ["tundra", "sequoia"]

[hosts.stonebox]
root     = "/home/morgan/work"
prefixes = ["*"]
```

```toml
# leaf: ~/.config/replicant/config.toml
home = "thistledown"
```

SSH keys are per host; a restricted host's key is additionally confined
server-side so it physically cannot read outside its prefixes.

## Conflicts

A conflict is a path both sides changed since the baseline. In the
home-base model that only happens when the discipline broke (edited on two
leaves without syncing between), so normal syncs produce zero prompts and
one line of counts. When conflicts exist they are loud, show both machines,
and the decision is manual.

### Unit of conflict

- **A git repo conflicts as a unit.** You cannot take one side's refs and
  the other's index. One prompt per repo:

  ```
  Repo conflict: tundra/tundra-beacon
    thistledown  bsneed/foo @ abc123  (2 dirty)   2026-10-31 09:12
    birchbook    main       @ def456  (clean)     2026-10-31 10:40

  Choices:
    Use thistledown's copy
    Use birchbook's copy
    Merge   (commit both sides, fetch peer-to-peer, merge here)
    Skip
  ```

- **Non-git paths conflict per file**, grouped by directory with
  "apply to all in this dir":

  ```
  File conflict between thistledown and birchbook: /work/blah/test1.txt
    thistledown  1.2K  2026-10-31 09:12
    birchbook    1.4K  2026-10-31 10:40  (newest)

  Choices:
    Use thistledown's copy
    Use birchbook's copy   [default]
    Skip
  ```

  `d` opens a diff for text files. Newest is the highlighted default;
  `--prefer newest` auto-resolves non-git conflicts without prompting.
  Git repos never auto-resolve.

### Collect, then apply

All questions are asked first, then a summary of what is about to happen,
then writes. Nothing is modified while deciding; Ctrl-C during the
questions is free.

### Skip is not neutral

A skipped path stays flagged in the baseline. The next `sync` re-asks,
`status` shows it in red, and the run ends with an unmissable block —
nonzero exit, red, path list — of everything left unresolved. Skip cannot
quietly become "ignore forever."

## Guardrails

Before any destructive apply, in order:

1. **Dirty target.** `git status --porcelain` on every repo on the target
   side. Anything that would be overwritten and isn't on the source → refuse
   (`--force` after printing exactly what dies).
2. **Stale baseline.** Push refuses if home moved since this leaf last
   pulled (someone else pushed). That is the "both changed" row, caught
   before rsync.
3. **Source mid-write.** Any `.git/index.lock` / `*.lock` on the source →
   refuse. (Alternative: capture git state via `git bundle`, which is
   consistent by construction. Evaluate.)
4. **Case collisions.** Two paths differing only by case headed from Linux
   home to a macOS leaf → refuse before rsync silently merges them. Only
   needed on pull.
5. **Prefix bounds.** A host's prefixes bound everything; paths outside
   them are never transferred, never deleted, in either direction.

`--dry-run` runs all five and prints the itemized rsync diff.

## Home is also the backup

Push is the only thing that writes to home, so a bad push is the one way to
damage the backup. Pushes run with `rsync --backup --backup-dir` into a
dated directory on home (or home runs filesystem snapshots). Either way, a
bad push is recoverable and the backup role of thistledown survives
replicant.

## Exclusions

Regenerable or per-machine; never synced, rebuilt locally:

```
target/  .build/  node_modules/  bin/  .DS_Store
.git/index.lock  .git/*.lock
```

Open question: `.git/index`. Syncing it is what caused failure 1 above, but
with a quiescent source and a full-tree copy the stat cache is merely stale,
not wrong, and `git status` refreshes it. Default to syncing it; revisit.

Worktrees: `.git/worktrees/*/gitdir` and a worktree's `.git` file hold
absolute paths. Either rewrite on arrival (`/Users/morgan` ↔ `/home/morgan`)
or require worktrees to live outside `~/work`. Decide in design.

## Implementation sketch

Rust crate, single binary, no daemon for v1. Same binary on home and
leaves; home just has `fleet.toml`.

```
src/
  main.rs        clap: sync | status | config
  config.rs      leaf config, fleet config, platform path resolution
  baseline.rs    per-leaf baseline read/write (both sides)
  state.rs       state hash: per-repo (HEAD, dirty hash) + manifest of non-repo files
  repos.rs       discover .git dirs under root; git status/lock/case checks
  decide.rs      the direction table
  conflict.rs    conflict detection, prompting, collect-then-apply, skip tracking
  guard.rs       the five refusals, with human-readable reports
  rsync.rs       build and run the rsync command, parse itemized output
  ssh.rs         remote exec over tailscale ssh (state, baseline, lock checks)
```

Tests: every guardrail and every row of the decision table gets a
fixture-repo test (dirty target, stale baseline, source lock, case
collision, prefix escape, repo conflict, file conflict, skip persistence).
rsync invocations tested against a temp dir pair; no network in unit tests.

## Phases

1. **Mitigation now.** Add `.git/index`, `.git/*.lock`, `.git/worktrees` to
   the Syncthing `.stignore`. Kills the most frequent symptom while replicant
   is built. (Does not fix ordering or concurrency.)
2. **v0.1.0 — `sync --dry-run` and `status`.** Config, discovery, state
   hash, decision table, rsync diff, guardrails 1/3/4/5. Read-only; proves
   the plan against the real tree.
3. **v0.2.0 — apply.** Real pull and push, baselines, guardrail 2,
   backup-dir on home.
4. **v0.3.0 — conflicts.** Repo and file conflict prompts, collect-then-
   apply, skip tracking, merge via peer-to-peer git.
5. **v0.4.0 — cutover.** Syncthing off for `~/work`. Keep it for anything
   outside.
6. **Later, if wanted.** Small daemon that notices a machine switch and runs
   `--dry-run` + notifies. It never applies on its own.

## Explicitly not doing

- Bidirectional / continuous sync. That is the bug.
- Forking Syncthing.
- A hub repo + second remote per project. Discussed; overkill. Peer-to-peer
  git covers the merge case.
- Writing our own file copier. rsync.
- Leaf-to-leaf sync. Everything goes through home.
- Two machines active on one repo at once. If that is needed for a specific
  repo, it gets a git-native workflow, not replicant.
- Auto-resolving git conflicts, ever.
