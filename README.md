# replicant

Explicit, home-mediated directory sync for a personal fleet of machines.
One command, no background magic.

Replicant keeps a working tree such as `~/work` identical across your own
machines, including every git repository's branches, HEAD, index, stashes and
uncommitted changes. It replaces continuous two-way sync with a deliberate
`replicant sync` that shows what it is about to do before it does it.

## Why

Continuous file sync and git do not mix. A repository's `.git/index` caches
one machine's filesystem state, refs can arrive before the objects they point
at, and two machines active in the same repository get last-write-wins instead
of a merge. Replicant treats each repository as one unit, decides a direction
for it, and moves it whole.

## How it works

- **One machine is home.** It is always on and holds the full tree. Every
  other machine is a leaf. A leaf talks only to home, never to another leaf.
- **A daemon on every machine.** Daemons talk to each other over mTLS. There
  is no ssh or rsync dependency; the daemons move the bytes themselves.
- **Sync is explicit.** `replicant sync` compares home, this machine and the
  state both agreed on last time, decides each unit's direction, prints the
  plan, and asks before applying it.
- **Units.** Each git repository is a unit; loose files outside repositories
  form units of their own. At a terminal, a conflict in one unit can be
  answered or skipped while the rest of the plan goes ahead.
- **Guardrails.** A sync refuses to overwrite work the other side has not
  seen, asks about each deletion by itself, and keeps what a push replaces on
  home in a backup so that `replicant rollback` can undo it.
- **Per-host trust.** Home runs a small CA. A new leaf starts with a bootstrap
  bundle that can only ask to be enrolled; `replicant approve` on home issues
  and pins that host's own certificate, optionally capped to an allow list of
  paths.

Git-ignored files do not sync.

## Requirements

- macOS or Linux
- Rust 1.88 or newer
- A network path from each leaf to home (a LAN, a tailnet, or both)

## Install

Clone the release your home runs and run the installer for the machine. It
builds replicant, installs it to `~/.cargo/bin`, and runs the daemon as a
service that starts with your session:

```sh
git clone --depth 1 --branch v0.4.13 https://github.com/sovran-la/replicant
sh replicant/scripts/install-linux.sh    # a systemd user service
sh replicant/scripts/install-macos.sh    # a launchd job
```

Every machine in a fleet should run the same version. Running an installer
again upgrades the binary and restarts the daemon; a unit file or plist that
is already there is left as it is.

On macOS the installer codesigns the binary with a fixed identifier when the
keychain holds an `Apple Development` certificate. The signature keeps Local
Network privacy from treating each rebuild as a new program. Without one it
installs unsigned, which works; macOS asks about the local network again
after each rebuild.

For the binary alone, with no service:

```sh
cargo install --git https://github.com/sovran-la/replicant --tag v0.4.13
```

## Quick start

State lives in `~/.sovran/replicant` (`REPLICANT_HOME` relocates it). A state
directory that holds `fleet.toml` is home; anything else is a leaf.
`docs/PLAN.md` describes `fleet.toml` and `leaf.toml`.

On home, run the daemon and print the bootstrap bundle:

```sh
replicant daemon
replicant config export-creds
```

On a new leaf, save that bundle as `~/.sovran/replicant/home.pem` and, with
the daemon running, ask for what the machine should hold:

```sh
replicant subscribe <home> ~/work
```

Back on home, review the request and decide its allow list:

```sh
replicant approve
```

Then, on the leaf:

```sh
replicant sync
```

## Commands

| Command | What it does |
| --- | --- |
| `daemon` | Run this machine's daemon (home if `fleet.toml` exists, else leaf) |
| `subscribe` / `unsubscribe` | Ask home to serve paths to this machine, or stop |
| `approve` | On home: review pending hosts and decide their caps |
| `status` | The decision table and open markers, without acting |
| `sync` | Decide each unit's direction, show it, and carry it out |
| `rollback` | On home: undo one push, or list the pushes |
| `log` | Read the journal |
| `host revoke` | On home: remove a host's certificate pin |
| `config show` | Where replicant keeps things and which role this machine has |

`replicant <command> --help` has the details, including `sync --dry-run`,
`--yes` and `--non-interactive` for unattended use.

## Status

Replicant is built for one person's machines and is used daily, but it is
young. Interfaces and the wire format change between releases. The design
record, including known limits, is in `docs/PLAN.md`.

## License

MIT. See [LICENSE](LICENSE).
