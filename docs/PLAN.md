# Replicant — Rough Plan

*Draft 2026-10-31. Captures the design discussion; nothing here is final.*

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

## Model: baton pass

One machine is active at a time. Switching machines *is* the sync.

```
replicant sync            # find the baton holder, make this machine match it, take the baton
replicant sync --from Y   # explicit source
replicant sync --dry-run  # show what this machine would lose / gain
replicant status          # who holds the baton, when, does this machine diverge
```

Transport is `rsync -a --delete` over Tailscale SSH. Replicant never copies
bytes itself; it is policy and guardrails around rsync.

There is no home base. The only shared state is the **baton record**:

```
holder     = twinoaks
since      = 2026-10-31T10:44:00-07:00
state      = <hash over per-repo (HEAD, dirty-tree hash) + non-repo tree manifest>
```

Every sync writes the record on both participants. Each machine also keeps
its own *last-agreed* state so it can tell whether it diverged after the
baton left.

### Divergence

Both machines changed since they last agreed → replicant refuses, lists the
repos/paths that diverged, and offers the git path:

```
replicant merge <repo>    # commit both sides, git fetch ssh://Y/<path>, merge here
```

Peer-to-peer git over ssh; no hub repo required.

## Guardrails (the whole safety story)

Before any destructive apply, in order:

1. **Dirty target.** `git status --porcelain` on every repo on the target.
   Anything that would be overwritten and isn't on the source → refuse
   (`--force` to override, after printing exactly what dies).
2. **Diverged target.** Target's current state ≠ its last-agreed state →
   refuse and point at `replicant merge`.
3. **Source mid-write.** Any `.git/index.lock` / `*.lock` on the source →
   refuse. (Alternative: capture git state via `git bundle` instead of raw
   copy; bundles are consistent by construction. Evaluate.)
4. **Case collisions.** Two paths differing only by case on a Linux source
   headed for a macOS target → refuse before rsync silently merges them.
5. **Allowlist.** Target's configured prefixes bound everything; paths
   outside them are never transferred, never deleted.

`--dry-run` runs all five and prints the rsync itemized diff.

## Exclusions

Regenerable or per-machine; never synced, rebuilt locally:

```
target/  .build/  node_modules/  bin/  .DS_Store
.git/index.lock  .git/*.lock
```

Open question: `.git/index`. Syncing it is what caused failures 1 above, but
with a quiescent source and a full-tree copy the stat cache is simply stale,
not wrong, and `git status` refreshes it. Default to syncing it; revisit.

Worktrees: `.git/worktrees/*/gitdir` and a worktree's `.git` file hold
absolute paths. Either rewrite on arrival (`/Users/morgan` ↔ `/home/morgan`)
or require worktrees to live outside `~/work`. Decide in design.

## Config

Per machine, `~/.config/replicant/config.toml`:

```toml
root     = "/Users/morgan/work"       # resolved per platform
machines = ["birchbook", "twinoaks", "stonebox", "spark1", "spruce"]
prefixes = ["*"]                      # twinoaks: ["tundra", "sequoia"]
exclude  = ["target", ".build", "node_modules", "bin", ".DS_Store"]
```

Machine names resolve over the tailnet. SSH keys are per machine; a
restricted machine's key can additionally be confined server-side so it
physically cannot read outside its prefixes.

## Implementation sketch

Rust crate, single binary, no daemon for v1.

```
src/
  main.rs        clap: sync | status | merge | config
  config.rs      load/validate, platform path resolution
  baton.rs       baton record read/write, last-agreed state
  state.rs       state hash: per-repo (HEAD, dirty hash) + manifest of non-repo files
  repos.rs       discover .git dirs under root; git status/lock/case checks
  guard.rs       the five refusals, with human-readable reports
  rsync.rs       build and run the rsync command, parse itemized output
  ssh.rs         remote exec over tailscale ssh (state, baton, lock checks)
```

Tests: every guardrail gets a fixture-repo test (dirty target, diverged
target, source lock, case collision, prefix escape). rsync invocations tested
against a temp dir pair; no network in unit tests.

## Phases

1. **Mitigation now.** Add `.git/index`, `.git/*.lock`, `.git/worktrees` to
   the Syncthing `.stignore`. Kills the most frequent symptom while replicant
   is built. (Does not fix ordering or concurrency.)
2. **v0.1.0 — `sync --from Y --dry-run`.** Config, discovery, rsync diff,
   guardrails 1/3/5. Read-only; proves the plan against the real tree.
3. **v0.2.0 — apply.** Real sync, baton record, guardrail 2, `status`.
4. **v0.3.0 — merge.** Divergence workflow via peer-to-peer git.
5. **v0.4.0 — cutover.** Syncthing off for `~/work`. Keep it for anything
   outside.
6. **Later, if wanted.** Small daemon that notices a machine switch and runs
   `--dry-run` + notifies. It never applies on its own.

## Explicitly not doing

- Bidirectional / continuous sync. That is the bug.
- Forking Syncthing.
- A hub repo + second remote per project. Discussed; overkill for the baton
  model. Peer-to-peer git covers the merge case.
- Writing our own file copier. rsync.
- Two machines active on one repo at once. If that is needed for a specific
  repo, it gets a git-native workflow, not replicant.
