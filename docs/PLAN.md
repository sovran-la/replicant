# Replicant — Rough Plan

*Draft 2026-10-31, rev 5 2026-11-01 (v0.1.0 shipped and deployed; review findings folded in; v0.2 entry criteria),
rev 6 2026-11-01 (decided: F5 weighted out-of-bounds rows, root-relative slash excludes, symlinked bases, granted leases),
rev 7 2026-11-01 late (decided: git-ignored files do not sync; approve mints per-host certs; backup/rollback/confirm shapes; v0.2 build list),
rev 8 2026-11-01 night (apply built on `bsneed/v0.2-apply`, pending review: F3's kill, sync applies, markers, home-side push backup, rollback per push, fleet view's last sync),
rev 9 2026-11-02 (apply review fixed: a marker resumes only its own unit; a resume will not replace leaf work newer than its marker (guardrail 6); a rolled-back push holds its pusher until `--accept-rollback`).
rev 10 2026-11-02 (sync shows each unit as it applies, with its time and the total; Ctrl-C at the CLI stops the running rsync, leaving its unit interrupted with its marker).
rev 11 2026-11-02 (a confirmed plan is what each unit will do and what it would delete, not the bytes it carries: an edit or a touch between the plan shown and the `y` no longer voids it).
rev 12 2026-11-03 (rsync removed: the daemons move the bytes over their mTLS link, the native transport; a dry run is the diff of both sides' manifests; home backs up by rename; per-unit progress; `[daemon] streams`. Built on `bsneed/v0.4-native`, release 0.4.0; record: `docs/v0.4-native-notes.md`).
rev 13 2026-11-03 (the v0.4-native review fixed: every unit diffs again and stops if that is not the plan, a pull too; sockets, FIFOs and devices are skipped on both sides; no frame too large and no panic can wedge a transfer; rollback is a round trip for directories and kind changes; a repo's index goes with its refs; home's `Done` is acked once its names are durable. Known limits restated. Record: "Review fixes" in the notes).
rev 14 2026-11-03 (the re-review fixed: a unit also stops if a path its plan lists holds anything else now, which rev 13 said and did not do; a read-only directory both sides agree about can be written into; Known limits restated. Record: "Re-review fixes" in the notes).
rev 15 2026-11-04 (addresses: a leaf's `[home] address` and home's `[daemon] listen` take a list; the dialer tries IPv4 first, skips unscoped link-local IPv6, and sticks with the address that answered. The first real push ran at 52 MB/s over Tailscale's userspace WireGuard and ~190 MB/s once the data path went over the LAN; `host.local` resolved to a link-local address first and cost each connection a 5 s connect timeout. Releases 0.4.1, 0.4.2; 0.4.3 after review: handshake bound, resolved-IP stickiness, skipped-address log, repeatable --address, one attempt per error line; 0.4.4 and 0.4.5 after the re-review: wire 7 and version-before-parse on both wires, scoped IPv6 kept, saved addresses in the subscribe reply, only a TLS verdict blames the bundle, and the test suite no longer hangs: the cut fixture shut down home's own socket from another thread, the macOS SO_RCVTIMEO wedge. Record: `docs/v0.4.2-addresses-review.md`).
rev 16 2026-11-05 (the review's second pass: every data-connection read goes through `native::polled`, a socket polled beside a pipe of its own, so a transfer stopped from another thread (an abort, a failed worker) ends its reads through the pipe rather than through `shutdown()`, which on macOS can leave a read wedged past its timeout (A21, A37); the cut fixture closes instead of shutting down (A30); `subscribe --address` reports what it replaced even when the request fails and not on a first subscribe (A28, A29); test flakes pinned (A23, A31, A39). Release 0.4.6).
rev 17 2026-11-05 (the review's third pass: 0.4.6 as tagged killed a leaf daemon with SIGPIPE on any failed transfer, because a stop raised after its connection was dropped wrote a pipe with no reader (A41): the pipe's read end is now shared with every stop handle, and the daemon never restores SIGPIPE's default disposition; the process raises its open-file limit at start, a connection costs fewer descriptors, and the suite passes at macOS's default 256 (A42); the sync watchdog is a guard that names the test and ends with it (A43); A39, A44-A50 and the file-size rule (A33). Release 0.4.7. Do not install 0.4.6.)
rev 18 2026-11-05 (guardrail 1 measures the working tree against HEAD plus untracked paths, not `git status`'s index column: an index synced from another machine reports staged changes whose files on disk are HEAD's, and refused a real push over them. The refusal names the machine whose work is at risk and what to do there. Release 0.4.8).
rev 19 2026-11-06 (`sync` asks "Apply this plan?" only when applying would do something: a transfer, a deletion, or a baseline to record or drop; a plan of settled units prints "nothing to apply" and exits 0. Release 0.4.9).
rev 20 2026-11-06 (guardrail 1's working-tree check reads only the files it must: rev 18's `git diff HEAD` re-hashed every file whose stat cache was off, which on a copied index is the whole repo, and a status on home went from 2.2 s to 5.3 s. The porcelain columns decide every entry but one shape, staged and then changed on disk, and only those files are hashed (`git hash-object`); no scratch index, no write. Release 0.4.10).
Captures the design discussion; nothing here is final. Rev 4 added the daemon control plane, FIFO queue + leases,
quarantine/rollback, journal, dogfood via --as. v0.1's working record is `docs/v0.1-notes.md`,
`docs/v0.1-review.md` and `docs/v0.1-fixes.md`; where they disagree, the review and fixes win.*

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

Each machine's replicant state lives in `~/.sovran/replicant`, beside the
other sovran daemons (`REPLICANT_HOME` relocates it). A directory holding
`fleet.toml` is home; anything else is a leaf.

**Trust: enrollment is the whole ceremony (v0.2).** Home's CA signs three
kinds of certificate, and home asks which one it is talking to:

- **Home's server certificate** (`server.pem`, server use only) never leaves
  home. A leaf also refuses a "home" certificate that would pass as a
  client, which shuts out v0.1's shared certificate posing as home.
- **The bootstrap** (`credentials.pem`, client use only), printed by
  `replicant config export-creds` and saved as a new leaf's `home.pem`. It
  can ask to be enrolled (`subscribe` from a host with no `[hosts.*]`
  entry) and collect the certificate `approve` issues. Home refuses
  anything else it sends.
- **A host's certificate**, minted by `approve` and pinned in fleet.toml as
  `[hosts.x] cert = "sha256:<DER fingerprint>"`. After the handshake, home
  hashes the peer's certificate and *that* is the host. The request's
  `host` field is not identity: a request naming another host is refused
  and journaled, and so is a certificate nobody pins. Fingerprint pinning
  avoids an X.509 parser.

The leaf keeps `home.pem` (its trusted CA) and stores its own certificate
as `host.pem`. `approve <host>` on an enrolled host reissues (the old pin
is replaced and the leaf collects the new one on its next command), and
`replicant host revoke <host>` removes the pin. There is no CRL, because
the pins *are* the list.

What remains: a pending entry is named by whoever holds the bootstrap, and
an issued certificate goes to the first bootstrap holder that collects it
under that name. If someone else got there first, the real leaf finds
nothing waiting, which is loud, and `approve` reissues. A pairing token is
not needed while every leaf is Brandon's. Caps still guard against mistakes
only until the ssh-side confinement exists.

**Data plane: the native transport** (rev 12; rsync over ssh until 0.3).
The daemons move the bytes themselves, over the same mTLS listener,
certificates and pins as the control plane; there is no ssh. Per unit:

- **Manifest.** Each side walks the unit into a manifest: (path, kind
  file/symlink/dir, size, mtime to the nanosecond, mode, link target), with
  the unit's excludes applied as it walks (root excludes, carve-outs, a
  loose unit's repos, worktree checkouts, and both sides' git-ignored
  paths). Symlinks are entries, never followed. The leaf walks its side;
  home sends its own (`HomeRequest::Manifest`, at the head of the queue).
- **Diff.** The diff of source against target *is* the dry run: an entry is
  sent when the target lacks it or holds it with another kind, size, mtime,
  mode or link target (a link compares by target only); what the target has
  and the source does not is deleted. Rendered as the same `+ / ~ / a / -`
  lines, it is what the plan shows, the guardrails read and the digest
  hashes.
- **Channel.** A data connection is a second kind of connection to home's
  listener: after the TLS handshake the leaf writes `replicant-data\n`
  where a request starts with `{`, then binary frames: `u32` payload length
  (big-endian), `u8` kind, payload (JSON for control-ish frames, raw bytes
  for file data; at most 8 MiB). The leaf says `Hello` (lease, unit,
  direction, ack timeout); home answers `Ready` or `Refused`. The leaf
  always dials and always drives: on a push it sends and home receives; on
  a pull it asks (`Get`) and home sends. Per unit it opens `streams`
  connections (the lower of home's `[daemon] streams` and the leaf's
  `[home] streams`, default 4, at least 1); the existing control link
  carries `BeginPush`, `Rehash`, `Agree`, `Settle`.
- **Acks and stalls.** Every frame is acked by its receiver, in order, up to
  8 in flight per connection. A side that waits longer than the ack timeout
  (60 s, carried in `Hello`) for an ack or a frame calls the connection dead,
  and the unit ends `Interrupted` (marker kept, no baseline, journaled), as a
  failed rsync did. Home checks that the lease is still the head of its queue
  before every frame of a push, and before every `Get` of a pull (a pull's
  reads harm nothing). An abort (a lost lease, the CLI gone) shuts every
  connection at once, and no failure, a panic included, leaves a transfer
  waiting on anything but a timeout.
- **Writes.** Small files (< 64 KiB) travel many to a frame, larger ones in
  1 MiB chunks, one file per worker (never split). The receiver writes
  `.replicant-tmp-<name>` in the file's directory, gives it the source's
  mode and mtime, fsyncs it and renames it over the target: a target file is
  never torn. Order per unit: kind changes and directories first, files (a
  repo's refs and index last), and only once every file arrived the deletions
  and the directories' final modes. Long lists go in several frames. On a push
  the leaf then waits for home's ack of its `Done`, sent once the names are
  fsynced. Then the leaf diffs both sides again: nothing may differ but
  directories still holding what the unit does not sync.
- **Fresh diff, either way** (rev 13, 14). Before anything of a unit is written,
  its marker included, it diffs again and stops ("it changed since the plan was
  made") unless the change list is the plan's and every path on it still holds
  what the plan saw there (kind, size, mtime, link): minutes can pass between
  the plan and a unit's turn, and on a pull a leaf has no backup. Rev 13 compared
  the lists alone, and an edit of a listed path reads the same in a list. A
  pull's receiver also lstats each path against that diff right before it
  overwrites or deletes it, and fails the unit if the path moved. What remains is
  in Known limits, "The window on a pull". A push's receiver is on home and backs
  up what it replaces.

### One command

```
replicant subscribe thistledown ~/work/tundra ~/work/sequoia   # enroll; what this machine wants
replicant unsubscribe ~/work/rust                              # stop; next sync cleans it off
replicant sync              # figure out the direction, show it, ask, do it
replicant sync --yes        # don't ask (deletions still need --confirm-delete,
                            # a rolled-back push --accept-rollback)
replicant sync --confirm-delete work/r   # carry out a "deleted on X" unit, or let a resume
                                         # replace what changed here since (repeatable)
replicant sync --accept-rollback work/r  # take home's copy of a unit whose push from here
                                         # home rolled back (repeatable)
replicant sync --non-interactive         # never prompt; without --yes, only shows the plan
replicant sync --dry-run    # decision + each unit's itemized diff, no writes
replicant sync --no-wait    # fail instead of queueing behind another sync
replicant status            # the decision table and open markers (with age); conflicts in red
replicant status --fleet    # every host: last contact, last sync, open markers, quarantines
replicant rollback [<push-id>]           # on home: undo one push; without an id, list them
```

Exit codes: 0 done (or nothing to do), 2 something needs a human and
nothing was written (a conflict, a quarantine, a guardrail refusal, an
unconfirmed deletion, no terminal and no `--yes`), 1 anything else (a
transfer that did not finish, a plan that changed after it was confirmed, a
`n` at the prompt).

Direction is inferred, never typed. Each leaf keeps a **baseline** per
unit: the state it and home last agreed on, recording both sides' hashes,
each compared only with its own side's next hash. Home keeps the same
baselines per leaf, plus the last N per path as history.

```
subscription base missing on its own side → error, never read as Empty
no baseline, target empty                 → pull (or push, whichever side has content)
no baseline, both populated, identical    → nothing to do
no baseline, both populated, different    → conflict; `--adopt home|leaf` is the bulk answer
home same,    leaf same                   → nothing to do
home changed, leaf same                   → pull   (make leaf match home)
home same,    leaf changed                → push   (make home match leaf)
unit vanished on one side since baseline  → "deleted on X"; never a plain pull/push,
                                            explicit confirmation required
both changed, to the same state           → nothing to do
both changed, differently                 → conflict resolution, see below
own marker for exactly this unit          → resume that direction: only on an exact-unit own
  (a dead pull here, a dead push on home)   marker (one above or below it is ignored, and the
                                            unit is decided by the rest of the table); a
                                            conflict if its source has vanished since; leaf
                                            paths newer than the marker that it would replace
                                            or delete block (guardrail 6, `--confirm-delete`)
home marker from another leaf             → quarantined (over the unit, above or below it);
                                            refused with the reason
home rolled back a push of mine, and the  → conflict "home rolled back your push <id>"; blocks
  table would write the leaf (pull, resume  until `--accept-rollback <path>` (`--yes` never
  pull, deleted on home)                    does), then decided by the table

baseline home no longer serves (home says why, per path):
  marker over it                          → conflict naming the interrupted sync; blocks
  unsubscribed, leaf == baseline.leaf     → remove from the leaf
  unsubscribed, leaf changed              → conflict: "push first, or
                                            `unsubscribe --force <path>`"; blocks
  cap narrowed, excluded, not served      → orphaned: shown, never deleted, never
                                            blocks; red if the leaf changed
  root missing or renamed on home         → "fix fleet.toml on <home>"; blocks
```

The two "identical" rows are what keep the Syncthing cutover from being a
wall of conflicts: both sides already agree, so agreeing is free. The
vanished and missing-base rows exist because "everything became Empty" is
far more often a renamed `~/work2`, an unmounted volume or an `--as` pointed
at nothing than a real delete, and reading it as an ordinary change sends a
whole-unit delete into home, the backup (review F4).

`Remove` is narrow for the same reason (F5, decided 2026-11-01): only a path
the host explicitly unsubscribed, with nothing unpushed (`leaf ==
baseline.leaf`) and no marker over it. Home tells the leaf why it no longer
serves each path the leaf has a baseline for (`unsubscribe` records what it
removed, `except` what it carved out), and the reason weights the row:

- **Unsubscribed with unpushed work** is a real conflict and blocks: the
  host asked for it gone, but deleting it would lose work.
- **A cap narrowed, an exclude added, a path dropped by hand** are home's
  policy, not this host's choice: the path is **orphaned**. It is shown
  (dim; red, "home will not accept them", if the leaf changed since
  baseline), it does not block the rest of the sync or the exit code, and
  nothing is deleted. `unsubscribe` removes it here; re-subscribing serves
  it again.
- **A root missing or renamed in fleet.toml** almost always means home's
  config is broken, not a decision, so it blocks: "fix fleet.toml on home".

Deleting unpushed leaf work in a loose unit is exactly what guardrail 1
cannot see, and a whole-unit delete because a cap was narrowed or a root
mistyped is the F4 failure again. Removing a loose unit excludes the repos
inside it, so it never takes a repo with it. A baseline holds hashes, not
manifests: the rows know *that* the leaf changed, not how many files did.
v0.1 has the identical, same-state, quarantine and these unserved rows; the
vanished row is new in v0.2-prep.

`sync` always prints what it decided before touching anything:

```
push → thistledown: 3 repos, 41 files changed, 0 deleted
```

### State hash

The table is decided per **unit**: one per repo found under a subscription
on *either* side (the union, so a repo only home has is still a unit), plus
one *loose* unit per subscription holding every non-repo entry under it. A
subscription that is itself a repo has no loose unit. Finer loose
granularity can come with v0.3's per-file conflicts.

- **Repo:** `.git/HEAD`, every ref (`for-each-ref`, including
  `refs/stash`), and the dirty tree: each porcelain v2 entry (staged content
  by blob id) plus the working-tree **metadata** of anything the index does
  not describe — size, mtime to the nanosecond, mode, link target. Never
  content (decided 2026-11-01): content cost 1.7 GB of reads per `status` on
  thistledown, and a modified 400 MB zip is still a modified 400 MB zip.
- **Git-ignored files inside repos do not sync** (decided 2026-11-01 late,
  reversing the rev 5 reading of F6). They are not state, not in the hash,
  never moved, so there is nothing to protect: F6 is deleted, not built.
  Only each repo's own `.gitignore` files count, never the global
  gitignore or `.git/info/exclude`. Untracked
  but not ignored still syncs: that is WIP. Consequence: `.ai-index/`,
  `target/`, `vectors.db` and every `.env` stay where they were made.
  Each side lists what its rules ignore (`repos::ignored`, `!negation`
  as git reads it, a tracked path never ignored), the lists cross the wire,
  and a unit's transfer leaves the union alone on both sides: a path ignored
  on either side is neither sent nor deleted (`docs/v0.2-prep-notes.md`,
  "Ignored files do not sync"; v0.4 notes).
- **Loose:** the sorted manifest of (path, kind, size, mtime, mode, link
  target).

Same contract everywhere: metadata, not content. A transfer gives each
file its source's mtime to the nanosecond and its mode, and each side compares only against its own baseline, so metadata
is enough and the hash need not be machine-independent. The one blind spot
is a same-size rewrite landing in the same nanosecond; it is pinned by a
test so it is a decision, not a surprise.

Known gaps (review Q3): the hash does not see `.git/config`, hooks,
`info/exclude`, reflogs (so stash entries below the top one), or
`skip-worktree`/`assume-unchanged` flags. It is over-sensitive to
`refs/remotes/*`, which IDE autofetch moves on every machine; dropping them is
a v0.2 entry criterion (they are refetchable, and a transfer moves them anyway).

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
birchbook does exactly this today: `~/work → ~/work2`.

A subscription base that is a **symlink is followed** (decided 2026-11-01,
review P4): `~/work2 → /Volumes/data/work2` is the directory it names, the
most likely `--as` layout. A dangling link is a missing base, so an
unmounted volume is the missing-base error once synced, never "empty".
Only the base is followed; symlinks inside the tree stay links, and a
transfer makes them as links.

`subscribe` is a request to home. Home records it in the fleet config and
starts serving it; `sync` on the leaf reads its subscriptions back from
home. Home is the record of truth: `replicant status` on thistledown shows
the whole fleet, and a leaf whose disk was wiped re-subscribes and gets
exactly what it had. `unsubscribe` removes a path; the next `sync` cleans
it off the leaf, under the conditions on the remove row and through the
dirty-target guard like any other delete. Unsubscribing a path inside a
broader subscription carves it out (`except = [...]`); re-subscribing
restores it.

### Config lives on home

```toml
# thistledown: ~/.sovran/replicant/fleet.toml

[roots.work]
path    = "/home/morgan/work"
exclude = ["target", ".build", "node_modules", ".DS_Store"]

[roots.claude]
path    = "/home/morgan/.claude"
# per-machine state, never synced: absolute-path slugs, sessions, credentials
exclude = ["projects", "todos", "statsig", "*.jsonl", ".credentials.json"]

[roots.sovran]
path    = "/home/morgan/.sovran"
# daemon state, locks, sockets, logs — including replicant's own state dir.
# A pattern with a "/" is relative to the root ("agents/state" is
# ~/.sovran/agents/state, whatever a host subscribes to); a bare name or a
# "**/" pattern matches at any depth. Subscribing inside an exclude is refused.
exclude = ["agents/state", "replicant", "**/*.sock", "**/*.lock", "**/*.log"]

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

`bin` is deliberately not in the work root's excludes: plenty of repos
track `bin/`, and a root-wide exclude would silently drop it. Add it per
root only where it is truly build output.

`allow` is an optional cap. Without it a host may subscribe to anything
home serves. With it, home refuses anything outside the cap, loudly — the
threat is not a hostile user but an agent or a reflex typing
`subscribe thistledown ~/work` on the wrong box. twinoaks gets a cap;
birchbook and stonebox do not.

Hand edits to fleet.toml are picked up on the next request (review F11). A
broken edit fails closed: every request that reads policy (subscribe,
subscriptions, state, acquire, approve) is refused until the file parses.
Heartbeats and releases read no policy (see *Leases*).

A leaf's local config is little more than where home is, and `subscribe`
writes that (plus `home_dir`, and optionally `[home] streams`, which can
only lower home's `[daemon] streams`). The v0.3 keys `rsync_host` and
`rsync_timeout_secs` are still read, ignored, and dropped on the next save.

### Addresses are routing, not trust (rev 15)

Every connection verifies the peer (home pins each leaf's certificate by
fingerprint; a leaf checks home's certificate against the CA it holds, the
fixed server name and server-only use), so where a daemon is reached is
only a matter of routing. Both sides take a list:

```toml
# birchbook: leaf.toml — the LAN name first, the tailnet address when away
[home]
name    = "thistledown"
address = ["thistledown.local:7750", "100.64.10.99:7750"]

# thistledown: fleet.toml — one listener per interface leaves arrive on
[daemon]
listen = ["192.168.10.199:7750", "100.64.10.99:7750"]
```

A bare string still works for either, and `subscribe --address` can be
repeated to write a list (one `--address` replaces the whole list; the reply
shows the list now saved and names what was dropped, even when the request
itself then fails). Home binds every listener or refuses to
start (a home answering on half its interfaces is a config mistake, not a
degraded start), and the unspecified address is still refused. A leaf tries
its list in order and then sticks with whichever answered, down to the IP it
answered at, for the rest of that CLI request; if that one fails it walks
the list again, and when none answers the error lists every attempt, one per
line. A listed address that was skipped on the way to one that answered is
logged by the daemon, so a sync quietly running over the slow path, or
something at the LAN address that is not home, leaves a trace. The TLS
handshake is bounded on its own (10 s, as on home) rather than by the
request deadline, so a peer that accepts TCP and says nothing costs seconds,
not the 900 s a request may take. Within one name, IPv4 is tried before
IPv6 and a link-local IPv6 address without a scope is never tried: mDNS
hands those back first for `host.local`, and without a scope the SYN goes
nowhere and the dial sat out the whole connect timeout per connection (that
was 0.4.0's 9.5 s per repo and 40 s dry runs).

Why it matters: Tailscale on macOS is userspace WireGuard, and its network
extension pegged a core at 52 MB/s. The same transfer over the LAN ran at
~190 MB/s on the big unit. Thistledown's LAN address is DHCP, so a leaf
names it by mDNS. Home's own `listen` entry is the literal address, and
since every listener must bind, a lease change means home refuses to start
on any interface until fleet.toml is edited; thistledown wants a DHCP
reservation, and if that is not enough `listen` can learn an interface name.

The macOS binary is codesigned at install (`Apple Development` identity,
identifier `la.sovran.replicant`): macOS Local Network privacy and Little
Snitch both key on code identity, and an unsigned rebuild is a new
identity to them, which cost 0.4.1's first minute of LAN connects.

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
  Its certificate (sha256:…) waits here; twinoaks collects it on its next request.
```

`approve` also mints the host's client certificate and pins it in
fleet.toml (see *Control plane*). The certificate waits on home until the
leaf's next request collects it over the bootstrap bundle. Collection is
one-shot, and from then on the leaf uses its own certificate. `approve` is
also where the leaf's SSH key will be authorized on home and confined to
its cap, if any (not built). Either way, enrollment is "subscribe on the
leaf, approve on home" and nothing else.

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

The daemon plans again when the person says y and writes only if that
plan is the one confirmed (rev 11): per unit, its path, kind, decision
(pull, push, resume, remove, deleted on X, conflict…), refusals and the
set of paths it would delete; and the refused subscriptions. Not the
bytes: a push or pull copies whatever is there when it runs, and a live
tree (an editor's autosave, `.DS_Store`, a `git status` rewriting an
index) always changes something in the seconds between the plan shown
and the `y`. A unit more or fewer, a decision flipped, or one path more
or fewer to delete, and nothing is written: "the plan changed after it
was shown", exit 1, run `sync` again to see the new one. A plan that may
not go at all (a conflict, a refusal) says why instead (exit 2).

While it writes, each unit is shown as it starts and as it ends (rev 10):
`[12/155] push  work/ai_stuff/bonsai … 220 files  1.4s`, or why it did not
finish, in red, and the run ends with `done: 155 units applied in 14m
02s`. `--json` streams one object per line, per unit event, then the
report. Ctrl-C while it writes stops the unit in progress the way a lost
lease does: its transfer is stopped, its marker stays (the next `sync`
resumes it), and no later unit begins. What finished before it stands.

### Skip is not neutral

A skipped path stays flagged in the baseline. The next `sync` re-asks,
`status` shows it in red, and the run ends with an unmissable block —
nonzero exit, red, path list — of everything left unresolved. Skip cannot
quietly become "ignore forever."

## Guardrails

Before any destructive apply, in order:

1. **Dirty target.** Uncommitted work on every repo on the target side:
   tracked paths whose working tree differs from HEAD, and untracked paths
   (ignored files never move, so they cannot be lost; rev 7). Not the
   index: a staged change whose file on disk matches HEAD is bookkeeping,
   and an index carried over from another machine calls files modified
   that are not (rev 18; thistledown's `tundra-tools` refused a push over
   six such phantoms). Decided from `git status`'s own columns; only an
   entry staged and then changed on disk is hashed, so a status reads no
   more than it did (rev 20). Anything that would be overwritten and isn't on the
   source → refuse, naming the machine and the remedy (`--force` after
   printing exactly what dies: not built yet). Stays active on resume.
2. **Stale baseline.** Push refuses if home moved since this leaf last
   pulled (someone else pushed). That is the "both changed" row, caught
   before any transfer.
3. **Source mid-write.** Any of git's own `*.lock` files inside `.git` on
   the source (`.git/index.lock` and friends; a `Cargo.lock` is source) →
   refuse. (Alternative: capture git state via `git bundle`, which is
   consistent by construction. Evaluate.)
4. **Case collisions.** Two paths differing only by case headed from Linux
   home to a macOS leaf → refuse before the case-folding filesystem merges them. Only
   needed on pull; applies whenever the leaf is macOS. v0.1 checks within
   each unit only; collisions *between* units (`work/Foo` vs `work/foo`, a
   repo vs a loose sibling) and NFC/NFD pairs are a v0.2 entry criterion
   (F7).
5. **Subscription bounds.** A host's subscriptions (and its `allow` cap)
   bound everything; paths outside them are never transferred, never
   deleted, in either direction.
6. **Changed since the interrupted sync** (rev 9, review A3). A resume
   (pull, removal, confirmed deletion) carries a decision made before it
   was interrupted, and its marker records only the source's state. Any
   leaf file or link its dry run would replace or delete whose mtime is
   after the marker's start → refuse, naming each path, until
   `--confirm-delete <unit>`. Covers a repo's working tree and `.git`,
   which guardrail 1 cannot see once the work is committed.

`--dry-run` runs them all and prints each unit's itemized diff.

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
hooks where a long wait would only become a timeout. `status` itself does
not take a queue entry; `sync` (including `--dry-run`) does.

### Leases

The head of the queue can die: a leaf loses power mid-push, an agent is
killed before post_work. Every entry carries a lease the holder heartbeats
to home's daemon. An entry that stops heartbeating is **expired**: journaled
loudly, removed from the queue, and the next entry proceeds.

Expiry releases the queue. It does **not** clear the marker (below). Those
are different things on purpose.

A lease lives on the length it was **granted** (decided 2026-11-01, review
P5): the fleet's `lease_secs` is read once, at `Acquire`, and stored on the
entry; heartbeats refresh by that and releases read nothing. So a typo saved
in fleet.toml mid-transfer refuses new work but cannot abort a sync in
flight (which, once a lost lease stops the transfer, it otherwise would).
An edit to `lease_secs` applies to the next `Acquire`.

Losing a lease is never silent. The holder retries heartbeats for as long
as the lease could still be alive; a definitive "not queued", or silence for
the whole lease, marks it lost and fails the run with "lease lost", whatever
the work returned. A failed release is an error too. A lost lease also
**stops the running transfer**: every data connection is shut at once, and
home itself refuses the next frame of a connection whose lease is no longer
the head of its queue, so two transfers never interleave on home (F3, built
in v0.2 for rsync, natively in 0.4). The unit's marker stays and the next sync resumes it.

A waiter's lease is currently refreshed without checking its connection, so
a leaf that dies while waiting holds its place for up to the full lease
(F8, v0.2 entry criterion).

## Interrupted sync

A transfer can die at 60%. Single files are never torn (the receiver writes
a temporary name and renames), and a repo's refs and index go last, after its
objects and worktree, but the *set* is incomplete. So:

- Before the transfer starts, the target side writes an **in-progress marker**
  for the path (direction, source host, source state, started-at).
- Baselines are written only after the transfer finished and its
  verification found nothing left to differ, and the marker is cleared in
  the same step.
- A marker on a **leaf** (dead pull) is private: home is untouched, nobody
  else can see the mess, and the leaf's next `sync` resumes the pull of
  exactly that unit. A marker on a loose unit says nothing about the repos
  inside it, nor a repo's about its loose unit (rev 9, review A1): those
  are decided by the table, or one exit-24 would pull home's copy over
  every leaf-only repo under a root.
- A marker on **home** (dead push) **quarantines that path**: pulls of it by
  any other leaf are refused with the reason, while syncs of other paths
  proceed. The queue keeps flowing; only the damaged path is fenced.

A home marker clears one of three ways:

- **Resume.** The pushing leaf's next `sync` sees home's marker for its own
  push, diffs and transfers again (what already arrived is not in the diff),
  finishes, writes the baseline,
  clears the marker. The common case; needs nothing from anyone. "Its own"
  is only as good as host identity, which is why per-host certs land before
  apply.
- **Rollback.** `replicant rollback <push-id>` on home restores the
  pre-push state from that push's backup plus its recorded itemized change
  list: new files removed, changed and deleted files restored (by home's
  own list operations, through the receiver), under a queue entry. Per push, not per path (decided rev 7).
  Baselines untouched. For when the leaf is not coming back soon and
  someone needs the path. The pushing leaf's next sync then holds the unit
  as a conflict rather than pull home's restored copy over the work it
  pushed, until it passes `--accept-rollback` (rev 9, review A2); the
  rollback settles once the two agree on the unit again, and its backup
  is kept until then.
- **Accept.** `--force` declares the half-state the new truth. Almost never
  right; exists so the other two are not the only exits.

`status` lists every open marker with its age; one older than `[daemon]
marker_alert_mins` (default 60) is red ("interrupted"), a younger one may
still be running. `--dry-run` shows what resuming would do. A resume whose
source has vanished since the marker was written is a conflict, never a
delete: the marker does not carry a deletion's confirmation. Nor does it
cover what the leaf wrote after the interruption: guardrail 6 holds the
resume until `--confirm-delete` (rev 9).

## Home is also the backup

Push and local agents are the only things that write home, so a bad push
is the one way to damage the backup from outside. As a push replaces or
deletes a path on home, what was there goes into
`~/.sovran/replicant/backup/<host>/<YYYYMMDDTHHMMSSZ>/run-<n>/` (rev 12): a
deleted path by rename, a replaced one by a hard link until the rename takes
its name (rev 13; a whole copy when the backup is on another filesystem), and
home keeps a manifest of the push (`state/pushes/`). The run's directory, and
`run-<n>.dirs` beside it for what each directory was (rev 13), are the record of
what it kept, so a run whose leaf died before reporting can still be rolled
back. `replicant rollback <push-id>` undoes
one push; backups older than `[daemon] backup_days` (default 14) are swept
daily, except a push whose marker is still open. A bad push is recoverable
and the backup role of thistledown survives replicant.

Until 0.3 home copied the files the push's own dry run listed before the
push wrote (openrsync, given `--backup`, silently skips `--delete`); the
rename is the receiver's, at the moment of the replace, so nothing is
listed in advance and nothing the push touches escapes the backup.

## Journal

Home's daemon keeps an append-only journal, one line per event. Timestamps
are UTC: std has no time zone database, and the journal is read from
several zones.

```
2026-10-31T19:04:11Z  sync      birchbook  push  work/tundra   3 repos 41 files 0 del  ok
2026-10-31T19:06:02Z  sync      twinoaks   pull  work/tundra   3 repos 41 files 0 del  ok
2026-10-31T19:30:40Z  hold      thistledown agent:beatrix work/tundra/tundra-beacon  acquired
2026-10-31T20:02:15Z  sync      birchbook  push  work/sequoia  interrupted (the other side closed the data connection)
2026-10-31T20:05:33Z  sync      birchbook  push  work/sequoia  resumed → ok
2026-10-31T20:10:00Z  conflict  birchbook  work/scratch/notes.md  chose:birchbook
2026-10-31T20:10:00Z  conflict  birchbook  work/rust/sysdirs      skipped
2026-10-31T21:00:00Z  approve   spruce     allow:(none)
2026-10-31T21:05:00Z  confirm-delete birchbook work/old  deleted on birchbook — would remove 3 files on thistledown; requires explicit confirmation: confirmed, carrying it out
2026-10-31T21:06:00Z  rollback  thistledown work/sequoia  push birchbook/20261031T200215Z: restored 2, removed 1
```

As built (v0.2): `sync` lines are per unit (`push 41 files 0 del ok
backup:<push-id>`, `push interrupted (<why>); marker kept`, `push 2
files 0 del resumed → ok`) plus one summary line per sync; `adopt` records
a shared state taken as the first baseline; `backup-sweep` the daily sweep.

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

A hold outlives the CLI process that took it, so something must keep it
alive. That something is the **agents daemon's view of the agent** (its pid
or agent status), not replicant: if replicant's own daemon heartbeated
holds, a killed agent would orphan its hold forever (review Q5).

### Per-path baselines make lending a repo cheap

Baselines and the direction table are per unit — per repo, and one loose
unit per subscription — not tree-wide. While twinoaks holds
`tundra/tundra-beacon`, birchbook can keep working on anything else; the
next `sync` is push for some paths and pull for others in one run. Touching
the lent-out repo on both machines puts that repo — and only that repo —
into conflict resolution.

### `--to <host>`

`replicant sync --to twinoaks` is sugar: push to home, then ask twinoaks's
daemon to run its `sync` (a pull from home). No data crosses leaf-to-leaf;
only the trigger does. Home still sees every byte and enforces every
subscription.

## Exclusions

Regenerable or per-machine; never synced, rebuilt locally. Built in, every
root:

```
.git/index.lock  .git/*.lock
.git/worktrees/
```

Everything else comes from each root's `exclude` in fleet.toml (`target/`,
`.build/`, `node_modules/`, `.DS_Store` in the example). Anchored (`/x`)
patterns are refused. The exclude engine is `globset` with the sovran
filesystem matching rules (decided 2026-11-01, landed in v0.2-prep); one
list per unit serves every walker, the transfer's manifests on both sides
included (rev 12: the rsync translation and its differential test are gone;
the accepted grammar is unchanged).

A pattern with a `/` is **relative to the root** (decided 2026-11-01, review
P3): `agents/state` on the `.sovran` root is `~/.sovran/agents/state` for a
host subscribed to `.sovran`, to `.sovran/agents`, or to anything else, so
what an exclude protects does not depend on how a host splits its
subscriptions. Per unit it is rewritten by stripping the unit's root-relative
prefix (`/state` in `.sovran/agents`; dropped in `.sovran/x`), and the same
rewritten list goes to discovery, the hash and the transfer. A bare name or a `**/`
pattern matches at any depth, as written. Subscribing to a path wholly
inside an exclude is refused; one written before the exclude existed is
orphaned, not served.

**`.git/index` is synced** (decided 2026-10-31, still true after v0.1).
Failure 1 above came from syncing it while both machines were live. With a
quiescent source and a full-tree copy the stat cache is merely stale, not
wrong, and the first `git status` on the receiver refreshes it. Not syncing
it would lose the staged/unstaged distinction, which is real state.

The cost of a stale stat cache is that `git status` rehashes every file
until something refreshes it. Replicant's git calls are read-only (no
optional locks, `core.fsmonitor=false`), so they never refresh it. **Home may
run a one-time `git update-index --refresh` on its own repos** (decided
2026-11-01): thistledown is nobody's working machine, and that write
changes only the cache, never staged state. Leaves get no such pass;
replicant does not write a leaf's `.git`.

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
The pattern was copied, not depended on: no shared sovran.la mTLS crate
exists.

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
  state.rs         state hash: per-repo (refs, dirty metadata, ignored manifest) + loose manifest
  repos.rs         discover .git dirs and worktrees under a root; git status/lock/case checks
  decide.rs        the direction table, per unit, including no-baseline and resume rows
  conflict.rs      conflict detection, prompting, collect-then-apply, skip tracking, non-interactive mode
  guard.rs         the five refusals, with human-readable reports
  native/          the data plane (rev 12): manifest, diff, frames, data
                   connections, sender, receiver, backup by rename
```

v0.1 as built diverges from this sketch in layout (`units.rs`, `plan/`,
`exclude.rs`, `cli/`, `ssh.rs` folded into `rsync.rs`, the daemon split by
role); `docs/v0.1-notes.md` has the table. The sketch stays as the map of
responsibilities.

Tests: every guardrail and every row of the decision table gets a
fixture-repo test (dirty target, stale baseline, source lock, case
collision, subscription escape, cap refusal, repo conflict, file conflict,
skip persistence, worktree detection, non-interactive conflict exit,
resume-from-marker, queue ordering, lease expiry, quarantine refusal, rollback, journal round-trip).
Transfers tested over socket pairs with home in-process (cut, stalled and
throttled connections injected) and between two real daemons over mTLS on
loopback; no tailnet in unit tests.

Performance: the plan's open question was whether state hashing across every
repo on both sides costs twenty seconds on `~/work`. Measured: the first real
`sync --dry-run` of `~/work → ~/work2` took **14.5 s**; after the 2026-11-01
perf work (dirty files by metadata, the loose-unit filter) it is **~1.1 s**.
Home logs per-snapshot timing — discover and unit phases, worker count, and
its five slowest units — so the next regression names itself (decided
2026-11-01). Discovery is going parallel (in flight).

## Known limits (rev 17)

- **Ending a blocked read from another thread** is done through a pipe the
  reader polls beside its socket (`native::polled`), never through
  `shutdown()` alone: on macOS a read blocked under `SO_RCVTIMEO` can miss
  the wakeup of its own socket's shutdown and sleep past its timeout
  (`AF_UNIX`: 5 wedged in 160,000 rounds under load; TCP: 0 in 48,000, which
  does not clear it). A write blocked on a full buffer still ends on its
  write timeout. The rule for this codebase: a socket read that another
  thread must be able to end goes through `Polled`, or through `poll(2)`
  with a wake pipe as `daemon::local` does. And a wake pipe that is written
  to (rather than closed, as `daemon::local`'s is) keeps its read end alive
  for as long as anything can write it: a write to a pipe with no reader is
  SIGPIPE, which 0.4.6 died of (A41). The daemon leaves SIGPIPE ignored, as
  the Rust runtime sets it; only the CLI restores the default, for `| head`.
- **Sockets, FIFOs and devices** are never listed, opened, sent or deleted, on
  either side: they count in the unit's notes (`--verbose`). A file whose name
  one holds on the target fails its unit, saying so; nothing replaces one.
- **`.replicant-tmp-*` is a reserved name.** A real file so named on a target is
  taken for a dead transfer's temporary: never listed, never sent, and removed
  at the next transfer of its unit, without a backup.
- **Hardlinks** arrive as independent files; **xattrs, ACLs and file flags**
  are not carried. Neither is in the state hash.
- **Directories' mtimes** are not kept (they are not state either); their
  modes are.
- **A symlink's own mode** is never compared or set (macOS keeps one, Linux
  has 0777): a link changes only by its target.
- **Case-insensitive targets:** guardrail 4 refuses a pull of names that differ
  only by case into a macOS leaf, as before. NFC/NFD pairs (F7) are still not
  checked. **A case-only rename** (home `foo.txt`, leaf `FOO.txt`) is not
  caught, since the guardrail sees home's names only, and takes three syncs to
  settle on such a leaf: the first stops at a deletion that would take what just
  arrived, the second lets it (both spellings then match) and the third sends
  the file under its new name. Home's copy is never at risk, and the leaf's old
  copy was the plan's to replace. Documented, not fixed (review m3); a directory
  rename whose old directory holds an excluded path may never settle (inspection,
  not run).
- **Names and link targets that are not UTF-8:** a name is skipped silently,
  subtree included, as discovery does; a link's target is carried lossily and
  both sides then compare equal, so verification passes while the link differs.
  Documented, not fixed (review m4).
- **The ack timeout also times the work.** A frame is acked after its work is
  done, so one taking over 60 s (a large file's fsync, an `EXDEV` copy, a frame
  of deletions on a slow disk) reads as a dead peer, and can do so every run;
  home keeps writing until that frame ends. Lists are split at about 1 MiB,
  which narrows it. Documented, not fixed (review m8).
- **A `.git` without `HEAD` is not a repo** (rev 12): what an interrupted pull
  of a new repo leaves is plain content until its refs arrive (last).
- **Durability:** each file is `fsync(2)`ed before its rename, each directory
  renamed into (or removed from) is fsynced at the end of the unit, and a push
  waits for that. So, as far as it goes:
  - *Process death, any platform:* a final name is the old file or the whole new
    one, never torn, and absent only where a kind change was under way (the old
    kind goes first; on home it is in the backup). A temporary is cleaned next
    run.
  - *Power loss on Linux, or a kernel panic on either:* the bytes of a renamed
    file are durable.
  - *Power loss on macOS:* `fsync(2)` is not `F_FULLFSYNC` (a drive-cache flush,
    4 ms a file), so a file can survive under its final name with the right size
    and mtime and **wrong bytes**, and nothing will ever look: the diff and the
    hash read metadata. Accepted for 0.4: the macOS side is a laptop with a
    battery, home is Linux, and rsync never fsynced at all. One `F_FULLFSYNC` per
    unit before its baseline would close it at about 4 ms a unit; not done.
- **No deltas:** a changed file is sent whole; a file is never split across
  connections, so one large file uses one stream.
- **A file changing while it is read** is not installed; the unit's
  verification then fails it (`Interrupted`) and the next sync sends it
  again. A live tree can do this every time, as rsync's exit 24 could (N4). A
  small file grown past 64 KiB since the diff is the same case.
- **The window on a pull:** from the plan to a unit's turn is closed, as far as
  metadata can tell (rev 14), and from there to each write the receiver lstats
  (rev 13). What is left: a change between a path's last lstat and its write is
  still overwritten; an edit that keeps both size and mtime to the nanosecond
  (`cp -p`, `tar x`, `touch -r` of the same length) looks like nothing, as it does
  to the diff itself. On a push, home's receiver is not held to a manifest during
  the transfer, since whatever it replaces is in the backup.
- **What stops a unit without risking work:** the comparison cannot tell an edit
  from a rewrite. Any `git` that rewrites a listed `.git/index` between the plan
  and the unit's turn stops that unit, as an editor's save of a listed path does.
  Nothing is written and the next `sync` goes, by a test. A real edit there comes
  back as a conflict, also by a test. Chosen over guessing which it was.
- **A read-only directory on the target** is opened for the transfer and has its
  mode back at the end (rev 14), and a transfer cut in between leaves it open: the
  next sync's diff shows that and repairs it. **The unit's own root** is in no
  manifest, so nothing knows a mode for it: a change directly under a read-only
  root fails its unit, naming the path, every run, by a test. Nothing is lost.
- **A rollback of a run that never reported** restores what its directory and
  notes hold; files it created stay, and if it turned a file into a directory,
  the rollback fails loudly with the original safe in `backup/`.

## Known data quality issues on home

thistledown's `~/work` is not clean: it carries Syncthing debris
(`.sync-conflict-*` files, including inside `.git/`), 22 GB of HuggingFace
blobs under `memoryco/gguf_models` (excluded for now), and large dirty
binaries (SegmentShop zips, xcframeworks). A cleanup pass before the first
real pull is a cutover prerequisite, and like cutover it is still deferred.

## Decisions 2026-11-01 (late)

- **Ignored files do not sync.** See *State hash*. F6 closed.
- **Enrollment is the whole trust ceremony.** The shared bundle every leaf
  starts with is a *bootstrap* credential good for exactly one thing: asking
  to be enrolled. `approve` mints the host's client cert, pins its
  fingerprint in `fleet.toml`, and the leaf's next request fetches the cert
  over the bootstrap bundle and stops using the bootstrap. Home derives the
  host from the peer certificate and ignores the self-asserted name. Home's
  server cert is separate from client certs. A pairing-token flow is not
  needed while every leaf is Brandon's.
- **Backup on push:** `~/.sovran/replicant/backup/<host>/<timestamp>/` on
  home (superseded twice: home's own copy in v0.2, a rename by the native
  receiver in rev 12) via `rsync --backup --backup-dir`, retained 14 days. `replicant
  rollback` is per push (the unit of "undo what just happened").
- **DeletedOn confirmation:** both forms — `--confirm-delete <path>` for
  hooks, and the y/N in the collect-then-apply pass for humans.
- **Second leaf is stonebox**, not twinoaks: lower stakes, and it exercises
  the Linux-leaf path (and F9's Mac→Linux push) before a work machine does.
- **Cleanup before the first real pull** happens on a list Brandon reads:
  `.sync-conflict-*` files (delete where the sibling original exists), and
  the largest dirty/untracked binaries by repo (exclude or move out).
- **F7 NFC/NFD** stays out of scope until it bites; no `unicode-normalization`
  dependency. Cross-unit case collisions remain a v0.2 item.
- **First real pull** is one subscription, when the apply path has been
  reviewed. Not yet.

### v0.2 build list, in order

1. Ignored files do not sync: `ignore` crate honoring in-repo `.gitignore`
   in the walker, rsync `--filter=':- .gitignore'` (or a materialized list if
   openrsync refuses it), differential test, hash and guardrail 1 unchanged
   apart from no longer seeing ignored paths.
2. Per-host certs through `approve`; bootstrap bundle demoted. Built on
   `bsneed/v0.2-certs`; see `docs/v0.2-prep-notes.md`, "Per-host
   certificates".
3. F3's second half: a lost lease kills the running rsync child. **Built**
   on `bsneed/v0.2-apply`.
4. Apply: real pull and push, baselines written after rsync exit 0,
   markers and resume, backup on push (home-side, see *Home is also the
   backup*), `--confirm-delete` and the y/N pass, `rollback`, journal
   entries. **Built** on `bsneed/v0.2-apply`, pending review; reviewed
   before it runs against `~/work`. Record: `docs/v0.2-prep-notes.md`,
   "Apply".
5. stonebox enrolled; F9's Mac→Linux push deletion names tested for real.

## Phases

1. **Mitigation now.** Add `.git/index`, `.git/*.lock`, `.git/worktrees` to
   the Syncthing `.stignore`. Kills the most frequent symptom while replicant
   is built. (Does not fix ordering or concurrency.)
2. **v0.1.0 — done.** Daemon skeleton, `subscribe`, `approve`, `status` (+
   `--fleet`), `sync --dry-run`: config, enrollment, discovery, state hash,
   decision table, rsync diff, guardrails 1/3/4/5. Read-only by construction.
   Merged 2026-10-31 after review (F1–F3, F10, F13, F14 and the nits fixed
   pre-merge), tagged `0.1.0`, 239 tests. Deployed: home on thistledown as a
   systemd user unit, leaf on birchbook via launchd, subscribed
   `~/work → ~/work2`. First real dry run 14.5 s, ~1.1 s after perf work.
3. **v0.2.0 — apply: built, pending review** (`bsneed/v0.2-apply`, not
   merged). Real pull and push into `~/work2` on a leaf (the `--as`
   dogfood), baselines on both sides, the queue + leases (a lost lease
   stops the rsync), markers/resume/rollback, journal, guardrail 2 (the
   both-changed row), home-side backup on push. Not built: per-path
   baseline history on home (the last N), `--force` accept of a marker.
   First real run is one subscription, after review.
4. **v0.3.0 — conflicts.** Repo and file conflict prompts, collect-then-
   apply, skip tracking, `--adopt`, merge via peer-to-peer git.
5. **v0.4.0 — the native transport** (rev 12, `bsneed/v0.4-native`, pending
   review): rsync removed; see the data plane above and
   `docs/v0.4-native-notes.md`.
6. **Agents** (the next release; planned as v0.4.0 before rev 12). Leaf hooks (sync), home hooks (hold/release),
   `--to`, `approve --sync`, pre-dispatch push on the agents side.
7. **Cutover.** Deferred; not designed yet. Lives in `~/work2` until then.
8. **Later, if wanted.** The daemon notices a machine switch and runs
   `--dry-run` + notifies. It never applies on its own.

## v0.2 entry criteria

Latent in a read-only v0.1, fatal once anything applies. Each is designed
before it is built (F-numbers from `docs/v0.1-review.md`). Decided and built
on `bsneed/v0.2-design` (rev 6), now in the body above: **F5** (the unserved
rows), and from `docs/v0.2-prep-review.md` **P3** (root-relative excludes),
**P4** (symlinked bases) and **P5** (granted leases).

- **F4** — a missing subscription base is an error, and a unit that vanished
  since baseline is "deleted on X" with explicit confirmation, never a plain
  whole-unit delete.
- **F6** — closed by rev 7: git-ignored files do not sync at all.
- **F7** — guardrail 4 also checks collisions across units and NFC/NFD pairs.
- **F8** — a waiter's lease refresh checks that its connection is alive.
- **F9** — (moot since rev 12: there is no rsync; deletions are the diff's
  own names) push deletions from openrsync to rsync 3 must come back with names;
  needs a real Mac→Linux push test with a deletion.
- **F11** — hand edits to fleet.toml are reloaded (also fixes `approve`
  previewing a stale cached fleet). Done in v0.2-prep.
- **F12** — per-host client certs, fingerprint-pinned in fleet.toml, host
  derived from the peer cert, server cert separated.
- **F3 (v0.2 half)** — a lost lease kills the running rsync child. Built
  on `bsneed/v0.2-apply`.
- **Q3** — drop `refs/remotes/*` from the state hash; IDE autofetch otherwise
  makes every repo a conflict.
- **Q5** — holds are kept alive by the agents daemon's view of the agent, not
  by replicant.

## Deferred on purpose

- **Cutover from Syncthing.** Dogfood in `~/work2` first; design the
  checklist when there is something to cut over to. The home cleanup pass
  (see *Known data quality issues on home*) is a prerequisite.
- **Secrets.** `.env`, `*.pem`, keys under `~/work` sync today over the
  tailnet and keep doing so. Nothing special until there is a reason.

## Explicitly not doing

- Bidirectional / continuous sync. That is the bug.
- Forking Syncthing.
- A hub repo + second remote per project. Discussed; overkill. Peer-to-peer
  git covers the merge case.
- Deltas. A changed file travels whole (decided 0.4). (Until 0.3 this line
  read "Writing our own file copier. rsync." Reversed in rev 12: most of
  the code was adapting rsync, which ran at ~40 MB/s over ssh with no
  progress and a guessed timeout.)
- Leaf-to-leaf sync. Everything goes through home.
- Two machines active on one repo at once. If that is needed for a specific
  repo, it gets a git-native workflow, not replicant.
- Auto-resolving git conflicts, ever.
