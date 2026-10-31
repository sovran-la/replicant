# Replicant — Rough Plan

*Draft 2026-10-31, rev 4 (daemon control plane, FIFO queue + leases, quarantine/rollback, journal, dogfood via --as).
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
Home may change by any means — a push from a leaf, or an agent working on
thistledown directly — and leaves cannot tell the difference, nor need to.

Every other machine is a leaf. A leaf talks only to home, never to another
leaf.

**Control plane:** a replicant daemon on every machine, daemon-to-daemon
over the tailnet with mTLS, the same pattern as sovran.la/agents remote
(no ssh dependency). The daemon handles subscribe/approve, state and
baseline exchange, the FIFO queue and leases, the journal, and triggers (`--to`,
`approve --sync`). It is what lets a brand-new leaf reach home *before* it
has an authorized key, which ssh-only could not do.

**Data plane:** `rsync -a --delete` over Tailscale SSH. Replicant never
copies bytes itself; it is policy and guardrails around rsync. The daemon
never carries file data.

### One command

```
replicant subscribe thistledown ~/work/tundra ~/work/sequoia   # enroll; what this machine wants
replicant unsubscribe ~/work/rust                              # stop; next sync cleans it off
replicant sync              # figure out the direction, show it, do it
replicant sync --yes        # don't wait for confirmation
replicant sync --dry-run    # decision + itemized rsync diff, no writes
replicant status            # the decision table without acting; unresolved conflicts in red
```

Direction is inferred, never typed. Each leaf keeps a **baseline** per
subscribed path: the state it and home last agreed on. Home keeps the same
baselines per leaf, plus the last N per path as history.

```
no baseline, target empty   → pull (or push, whichever side has content)
no baseline, both populated → conflict; `--adopt home|leaf` is the bulk answer
home same,    leaf same     → nothing to do
home changed, leaf same     → pull   (make leaf match home)
home same,    leaf changed  → push   (make home match leaf)
both changed                → conflict resolution, see below
interrupted marker present  → resume that direction; never a conflict
```

`sync` always prints what it decided before touching anything:

```
push → thistledown: 3 repos, 41 files changed, 0 deleted
```

State is a hash over per-repo `(HEAD, dirty-tree hash)` plus a manifest of
non-repo files, scoped to the leaf's subscriptions.

### Subscriptions

A leaf declares what it wants with `subscribe`: home, plus one or more
paths. A path is anything under a root home serves — a directory or a
single file — expressed as the leaf's local path; home maps it through its
own root table (`/Users/morgan` ↔ `/home/morgan`). Roots are plural:
`~/work` is the big one, but `~/.claude` and `~/.sovran` carry config that
should be identical everywhere too.

```
replicant subscribe thistledown ~/work/tundra ~/work/sequoia
replicant subscribe thistledown ~/.claude/skills ~/.claude/commands ~/.claude/settings.json
replicant subscribe thistledown ~/.sovran/agents/types ~/.sovran/agents/hooks
replicant subscribe thistledown ~/work/tundra --as ~/work2/tundra   # local path override
```

`--as` maps a home path to a different local path; default is identity.
This is how we dogfood: `~/work` stays on Syncthing untouched while the
same content is subscribed into `~/work2` and lived in for a while.

`subscribe` is a request to home. Home records it in the fleet config and
starts serving it; `sync` on the leaf reads its subscriptions back from
home. Home is the record of truth: `replicant status` on thistledown shows
the whole fleet, and a leaf whose disk was wiped re-subscribes and gets
exactly what it had. `unsubscribe` removes a path; the next `sync` cleans
it off the leaf, through the dirty-target guard like any other delete.

### Config lives on home

```toml
# thistledown: ~/.sovran/replicant/fleet.toml

[roots.work]
path    = "/home/morgan/work"
exclude = ["target", ".build", "node_modules", "bin", ".DS_Store"]

[roots.claude]
path    = "/home/morgan/.claude"
# per-machine state, never synced: absolute-path slugs, sessions, credentials
exclude = ["projects", "todos", "statsig", "*.jsonl", ".credentials.json"]

[roots.sovran]
path    = "/home/morgan/.sovran"
# daemon state, locks, sockets, logs
exclude = ["agents/state", "**/*.sock", "**/*.lock", "**/*.log"]

[hosts.birchbook]
home_dir = "/Users/morgan"

[hosts.twinoaks]
home_dir = "/Users/morgan"
allow    = ["work/tundra", "work/sequoia"]   # subscribe may narrow this, never widen it

[hosts.stonebox]
home_dir = "/home/morgan"

# written by `subscribe`, not by hand
[subscriptions.birchbook]
paths = ["work", ".claude/skills", ".claude/commands", ".claude/settings.json",
         ".sovran/agents/types", ".sovran/agents/hooks"]

[subscriptions.twinoaks]
paths = ["work/tundra", "work/sequoia"]
```

`allow` is an optional cap. Without it a host may subscribe to anything
home serves. With it, home refuses anything outside the cap, loudly — the
threat is not a hostile user but an agent or a reflex typing
`subscribe thistledown ~/work` on the wrong box. twinoaks gets a cap;
birchbook and stonebox do not.

A leaf's local config is nothing but where home is, and `subscribe` writes
that. SSH keys are per host; a capped host's key is additionally confined
server-side so it physically cannot read outside its cap.

### Enrolling a new machine

A `subscribe` from a host with no `[hosts.*]` entry is recorded as
**pending**: home stores the host, its `home_dir` (learned from the leaf's
`$HOME`), and the requested paths, serves nothing, and tells the leaf what
happens next. Auto-enrolling with full access was considered and rejected:
it is exactly how twinoaks would end up with `rust/` on it, because the cap
did not exist yet when it asked.

On home, `replicant approve` lists what is waiting and forces the one
decision that matters — whether this box gets a cap:

```
$ replicant approve
Pending:
  twinoaks   /Users/morgan   wants: work/tundra, work/sequoia   (2m ago)

Approve twinoaks? [y/N] y
Allow list (relative to home dir, space-separated, empty = anything):
> /Users/morgan/work/sequoia ~/work/tundra
  → work/sequoia, work/tundra
Confirm? [Y/n] y
Approved. Run `replicant sync` on twinoaks to pull.
```

`approve` is also where the leaf's SSH key gets authorized on home (and
confined to its cap, if any), so enrollment is "subscribe on the leaf,
approve on home" and nothing else.

`approve` does **not** start a sync. Home never initiates writes to a leaf;
the first pull onto a fresh machine is the largest, least-reviewed sync it
will ever do and you want to be sitting at it. `approve <host> --sync` is
the opt-in: it triggers the leaf's own `sync` over ssh, the same way `--to`
does.

### Path normalization

One normalizer serves `subscribe`, `unsubscribe`, `approve`, and `allow`,
so a path means the same thing on both ends:

- `~/x` → `x`
- absolute under the *leaf's* `home_dir` (`/Users/morgan/…`) → stripped
- absolute under *home's* own home dir (`/home/morgan/…`, typed on
  thistledown from muscle memory) → stripped
- trailing slashes dropped, duplicates collapsed, `..` refused
- anything not under a configured root → refused, with the roots listed

The normalized list is echoed back before it is written, so what was
approved is what was seen, not what was typed.

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
5. **Subscription bounds.** A host's subscriptions (and its `allow` cap)
   bound everything; paths outside them are never transferred, never
   deleted, in either direction.

`--dry-run` runs all five and prints the itemized rsync diff.

## The queue

Home's daemon keeps one FIFO queue. Every sync is an entry; a sync waits
until every entry ahead of it is completely done. Nothing interleaves on
home, ever. Per-path locking was considered and shelved: the fleet is small
enough that strict serialization costs nothing noticeable, and one
invariant beats two. It stays in the back pocket as an optimization.

Agent work on home is an entry too — a **hold** rather than a sync (see
*Agents on home*). Same queue, same rules; an agent and a sync can never
overlap on home.

`status` on any machine shows the queue ("waiting: behind twinoaks pull
work/tundra, started 40s ago"). `--no-wait` fails instead of queueing, for
hooks where a long wait would only become a timeout.

### Leases

The head of the queue can die: a leaf loses power mid-push, an agent is
killed before post_work. Every entry carries a lease the holder heartbeats
to home's daemon. An entry that stops heartbeating is **expired**: journaled
loudly, removed from the queue, and the next entry proceeds.

Expiry releases the queue. It does **not** clear the marker (below). Those
are different things on purpose.

## Interrupted sync

rsync can die at 60%. Single files are never torn — rsync writes to a temp
name and renames — but the *set* is incomplete, and for a repo that can
mean refs pointing at objects that never arrived. So:

- Before rsync starts, the target side writes an **in-progress marker**
  for the path (direction, source host, source state, started-at).
- Baselines are written only after rsync exits 0, and the marker is
  cleared in the same step.
- A marker on a **leaf** (dead pull) is private: home is untouched, nobody
  else can see the mess, and the leaf's next `sync` resumes the pull.
- A marker on **home** (dead push) **quarantines that path**: pulls of it by
  any other leaf are refused with the reason, while syncs of other paths
  proceed. The queue keeps flowing; only the damaged path is fenced.

A home marker clears one of three ways:

- **Resume.** The pushing leaf's next `sync` sees home's marker for its own
  push, reruns the same rsync (idempotent), finishes, writes the baseline,
  clears the marker. The common case; needs nothing from anyone.
- **Rollback.** `replicant rollback <path>` on home restores the pre-push
  state from that push's `--backup-dir` plus its journaled itemized change
  list: new files removed, changed and deleted files restored. Baselines
  untouched. For when the leaf is not coming back soon and someone needs
  the path.
- **Accept.** `--force` declares the half-state the new truth. Almost never
  right; exists so the other two are not the only exits.

A marker older than a configurable age is reported in `status`, and
`--dry-run` shows what resuming would do.

## Home is also the backup

Push and local agents are the only things that write home, so a bad push
is the one way to damage the backup from outside. Pushes run with
`rsync --backup --backup-dir` into a dated directory on home (or home runs
filesystem snapshots). Either way, a bad push is recoverable and the backup
role of thistledown survives replicant.

## Journal

Home's daemon keeps an append-only journal, one line per event:

```
2026-10-31T12:04:11-07  sync      birchbook  push  work/tundra   3 repos 41 files 0 del  ok
2026-10-31T12:06:02-07  sync      twinoaks   pull  work/tundra   3 repos 41 files 0 del  ok
2026-10-31T12:30:40-07  hold      thistledown agent:beatrix work/tundra/tundra-beacon  acquired
2026-10-31T13:02:15-07  sync      birchbook  push  work/sequoia  interrupted (rsync exit 20)
2026-10-31T13:05:33-07  sync      birchbook  push  work/sequoia  resumed → ok
2026-10-31T13:10:00-07  conflict  birchbook  work/scratch/notes.md  chose:birchbook
2026-10-31T13:10:00-07  conflict  birchbook  work/rust/sysdirs      skipped
2026-10-31T14:00:00-07  approve   spruce     allow:(none)
```

`replicant log [--host H] [--path P] [--since T]` reads it. Leaves keep a
local copy of their own entries so the history survives home being
unreachable. Together with per-path baseline history this answers "what
did home look like before that push" and "which sync did that."

## Fleet view

Home's daemon already knows the whole world: enrolled hosts and caps, each
leaf's per-path state against its baseline (so the direction table for
*every* machine, not just this one), the queue and in-flight leases,
markers and quarantines, unresolved conflicts, the journal. The daemon
exposes all of it as one query, `fleet_state`, so every renderer is thin.

The one renderer in scope: **`replicant status --fleet`** from any
machine. One row per host: last contact, pending direction per path, queue
position; red for conflicts, quarantine, expired leases. Zero new
infrastructure.

An MCP panel and a pane in the sovran dashboard app are deliberately out
of scope. If this works for us they get built later and separately, on
top of the same `fleet_state` query. Not a web UI served by the daemon,
then or now: it would be the first HTTP surface on any sovran daemon and
a second UI stack.

The fleet view carries one thing `status` does not: **staleness**. A leaf
that has not talked to home in nine days looks different from one that
synced an hour ago, because "spruce is quietly diverging" is exactly what
you want to see from birchbook and cannot today.

## Agents integration

Agent-driven syncs run through the sovran.la/agents daemon's existing
lifecycle hooks (added 2026-08-26, motivated by exactly this). Nothing new
in the daemon's mechanism; the hook scripts call replicant. This works on
every machine the agents MCP is installed on.

```
~/.sovran/agents/hooks/pre_work.sh    →  replicant sync --yes --non-interactive
~/.sovran/agents/hooks/post_work.sh   →  replicant sync --yes --non-interactive
```

Handing work to an agent on another machine is then four syncs, two of
which are automatic:

1. birchbook `sync` — push to home *(you, or the local daemon pre-dispatch)*
2. twinoaks `pre_work` — pull from home *(daemon)*
3. twinoaks `post_work` — push to home *(daemon, on every terminal state)*
4. birchbook `sync` — pull from home *(you, whenever)*

### Contract with the daemon

- **Hooks never prompt.** `--non-interactive` (or no tty): any conflict is a
  nonzero exit with the full report on **stderr**. The daemon captures hook
  stderr to a file and discards stdout, so stderr is the only channel.
- **Pre_work failure aborts the spawn**, and the daemon's `HookFailed`
  error carries the exit code plus a tail of stderr. The calling agent sees
  *why* in the `agent_spawn` failure response — "Repo conflict:
  tundra/tundra-beacon …" — rather than running on a mystery tree.
- **Post_work failure is recorded on the agent** via
  `registry.record_hook_error` and shows in `agent_status`. It cannot change
  the agent's outcome (arch.md rule), but it cannot be missed either.
  Replicant's own `status` on the leaf also shows "changed, not pushed".
- **The one-agent-per-directory lock is held during hooks**, which doubles
  as the guarantee nothing else on that machine writes the tree while
  replicant copies it.

### Remote spawns

For `agent_spawn host=twinoaks`, step 1 must happen on the *spawning* side
before the daemon routes the request, otherwise the remote pre_work pulls
whatever home had rather than what the spawner has. Small addition to the
agents daemon: run the local pre_work (or a dedicated pre_dispatch) before
forwarding a remote spawn. Tracked in sovran.la/agents, not here.

### Agents on home

thistledown runs the agents daemon too, and agents may work there
directly. That is not a drift problem: baselines are leaf↔home, and *how*
home changed is invisible to a leaf — an agent committing on thistledown
is just "home changed → pull" on the next sync. The only hazard is
concurrency: an agent writing `tundra-beacon` on home while twinoaks pulls
it.

So on home the hooks are queue holds, not syncs:

```
pre_work   →  replicant hold    <working_dir>   # enqueues; waits its turn like any sync
post_work  →  replicant release <working_dir>   # journal entry: what the agent touched
```

Syncs queued behind a hold wait (or `--no-wait` fails) and `status` names the
agent holding it. The daemon on home decides which hook script applies by
knowing it is home; the leaf hooks are the sync ones above.

### Per-path baselines make lending a repo cheap

Baselines and the direction table are per repo and per non-git directory,
not tree-wide. While twinoaks holds `tundra/tundra-beacon`, birchbook can
keep working on anything else; the next `sync` is push for some paths and
pull for others in one run. Touching the lent-out repo on both machines
puts that repo — and only that repo — into conflict resolution.

### `--to <host>`

`replicant sync --to twinoaks` is sugar: push to home, then ask twinoaks's
daemon to run its `sync` (a pull from home). No data crosses leaf-to-leaf;
only the trigger does. Home still sees every byte and enforces every
subscription.

## Exclusions

Regenerable or per-machine; never synced, rebuilt locally:

```
target/  .build/  node_modules/  bin/  .DS_Store
.git/index.lock  .git/*.lock
.git/worktrees/
```

**`.git/index` is synced** (decided 2026-10-31). Failure 1 above came from
syncing it while both machines were live. With a quiescent source and a
full-tree copy the stat cache is merely stale, not wrong, and the first
`git status` on the receiver refreshes it. Not syncing it would lose the
staged/unstaged distinction, which is real state.

**Worktrees are not synced** (decided 2026-10-31). They are short-lived and
only matter to the agent using them. Replicant detects them rather than
relying on a path convention: a worktree is any directory whose `.git` is a
*file* pointing into some repo's `.git/worktrees/`. Both ends are excluded —
the checkout directory and the parent's `.git/worktrees/<name>/`. The
worktree's commits still travel, because its branch ref lives in the
parent's `refs/heads/` and its objects in the shared `.git/objects/`. Only
the disposable checkout stays machine-local.

## Implementation sketch

Rust crate, single binary: CLI plus daemon mode (`replicant daemon`),
same binary on home and leaves. Home is whichever daemon has `fleet.toml`.
Daemon-to-daemon transport reuses the sovran.la/agents remote pattern
(mTLS over plain TCP on the tailnet, `std::thread` + channels, no Tokio).

```
src/
  main.rs          clap: subscribe | unsubscribe | approve | sync | status | log | hold | release | rollback | daemon | config
  config.rs        leaf config, fleet config (roots, hosts, allow, subscriptions), home_dir mapping
  daemon/
    mod.rs         listener, request dispatch, home-vs-leaf role, fleet_state query
    remote.rs      mTLS client/server, the agents remote pattern
    queue.rs       the FIFO: syncs and holds, leases + heartbeats, expiry
    journal.rs     append-only journal; leaf-local copy of own entries
  subscribe.rs     subscribe/unsubscribe requests; pending hosts, approve, cap enforcement on home
  paths.rs         the one path normalizer (leaf and home)
  baseline.rs      per-leaf, per-path baseline read/write; last-N history on home
  marker.rs        in-progress markers; home-side quarantine; resume / rollback / accept
  state.rs         state hash: per-repo (HEAD, dirty hash) + manifest of non-repo files
  repos.rs         discover .git dirs and worktrees under a root; git status/lock/case checks
  decide.rs        the direction table, per path, including no-baseline and resume rows
  conflict.rs      conflict detection, prompting, collect-then-apply, skip tracking, non-interactive mode
  guard.rs         the five refusals, with human-readable reports
  rsync.rs         build and run the rsync command, parse itemized output
  ssh.rs           the data-plane ssh invocation only; control goes through the daemon
```

Tests: every guardrail and every row of the decision table gets a
fixture-repo test (dirty target, stale baseline, source lock, case
collision, subscription escape, cap refusal, repo conflict, file conflict,
skip persistence, worktree detection, non-interactive conflict exit,
resume-from-marker, queue ordering, lease expiry, quarantine refusal, rollback, journal round-trip). rsync
invocations tested against a temp dir pair; daemon-to-daemon tested over
loopback; no tailnet in unit tests.

Performance note: state hashing runs `git status` across every repo on
both sides per sync. Might be fine, might be twenty seconds on `~/work`.
v0.1's `--dry-run` is where we measure; caching by mtime is the fallback.

## Phases

1. **Mitigation now.** Add `.git/index`, `.git/*.lock`, `.git/worktrees` to
   the Syncthing `.stignore`. Kills the most frequent symptom while replicant
   is built. (Does not fix ordering or concurrency.)
2. **v0.1.0 — daemon skeleton, `subscribe`, `approve`, `status` (+ `--fleet`),
   `sync --dry-run`.** Config, enrollment, discovery, state hash, decision
   table, rsync diff, guardrails 1/3/4/5. Read-only; proves the plan
   against the real tree and measures the state-hash cost.
3. **v0.2.0 — apply.** Real pull and push into `~/work2` on a leaf (the
   `--as` dogfood), baselines + history, the queue + leases, markers/resume/rollback, journal,
   guardrail 2, backup-dir on home.
4. **v0.3.0 — conflicts.** Repo and file conflict prompts, collect-then-
   apply, skip tracking, `--adopt`, merge via peer-to-peer git.
5. **v0.4.0 — agents.** Leaf hooks (sync), home hooks (hold/release),
   `--to`, `approve --sync`, pre-dispatch push on the agents side.
6. **Cutover.** Deferred; not designed yet. Lives in `~/work2` until then.
7. **Later, if wanted.** The daemon notices a machine switch and runs
   `--dry-run` + notifies. It never applies on its own.

## Deferred on purpose

- **Cutover from Syncthing.** Dogfood in `~/work2` first; design the
  checklist when there is something to cut over to.
- **Secrets.** `.env`, `*.pem`, keys under `~/work` sync today over the
  tailnet and keep doing so. Nothing special until there is a reason.

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
