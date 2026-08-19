# The instance data root

Where one host keeps its state, who is allowed to write it, and what happens
when two processes want the same directory.

Split out of [`storage.md`](storage.md), which is at the repository's 500-line
ceiling. That file describes the *layout* inside the root; this one describes
the root itself.

## Resolution order

`src/store/paths.rs::resolve_home` and `src/app/config.rs::data_dir_from` answer
the same question and must agree — a host whose bundles and workspace resolved
differently would split its own state in half.

1. `--home` (the flag outranks everything)
2. `OPENCOMPANY_DATA_DIR` — what the platform manager injects into every tenant
   container
3. `$HOME/.opencompany`
4. `$USERPROFILE\.opencompany` — Windows sets this and not `HOME`
5. `.opencompany`, relative

Step 4 exists because step 5 is dangerous rather than merely inelegant. A
relative root resolves against the process working directory, which for a
double-clicked application is wherever the launcher happened to put it —
plausibly `C:\Program Files`, plausibly unwritable, and plausibly *different*
between launches. Two runs would quietly use two stores. `HOME` still wins where
both are set: git-bash and MSYS set both, and a user who exported `HOME` meant
it.

`OPENCOMPANY_HOME` is refused loudly rather than ignored — see `paths.rs`.

## Single writer, enforced

The runtime journal is single-writer. Two processes over one root overwrite each
other's companies, and until `src/store/lock.rs` existed nothing stopped them —
`resolve_home` handed the same directory to every caller that asked.

On a server that was survivable, because a second `opencompany serve` against
one root is a deliberate act. A desktop application is different in kind: it is
launched by double-clicking, and being launched twice is ordinary — two windows
of the installed app contend for one root with nobody having decided to do
anything.

A terminal `serve` and the desktop app do **not** collide by default: they
resolve different roots (see [the desktop root](#the-desktop-root-is-not-the-cli-root)
below). They collide when something points both at one directory — most often an
exported `OPENCOMPANY_DATA_DIR`, which is exactly how a developer runs the two
against the same companies on purpose.

`serve` and `app::boot::prepare_instance` both take an exclusive advisory lock
on `<root>/.lock` (`flock`/`LockFileEx` via `fs2`) and hold it for the life of
the process. A second instance is refused immediately with a message naming the
directory and `OPENCOMPANY_DATA_DIR`.

Since issue #726 the journal's *bytes* may live in the storage backend rather
than under this root (see [journal.md](journal.md)), and that changes nothing
here: single-writer-per-company is still the contract. Two live hosts on one
tenant database no longer corrupt each other's records — sequences are allocated
server-side, so appends interleave without collision — but each keeps its own
in-memory replay of the executed-key set, so neither sees the other's commits
until it reloads. That gap is pre-existing and unchanged; the root lock above is
still what a single-node deployment relies on.

An OS advisory lock rather than a pid file: the lock belongs to the open file
description, so the kernel drops it when the process exits for any reason —
clean exit, panic, `SIGKILL`, power loss. There is no stale state to detect and
nothing for an operator to delete by hand. The lock file itself is created if
absent and never removed; deleting it on release would race a second process
that has already opened the same path.

Scope: this is a *process* boundary on one machine. Two hosts over a network
filesystem are outside what `flock` promises, and that layout was never safe to
share regardless.

### Hosted deployments: the overlap window

**Answered — a tenant rollout cannot overlap**
(`opencompany-microservice`#15). In hosted mode the manager runs each tenant as
a container over `OPENCOMPANY_DATA_DIR=/data`, and the question was whether a
rollout ever has the new pod running while the old one still holds the volume —
which since this lock exists means the new pod fails to boot where it
previously started and silently raced.

It does not, and by construction rather than by configuration. A tenant is a
**StatefulSet at one replica**, and a StatefulSet has no `maxSurge`: pod
`opencompany-0` is deleted and fully terminated before its replacement is
created, so at most one pod ever holds the claim. The one path that rolls
tenants — `kubectl rollout restart statefulset -A -l app=opencompany` in the
platform's deploy workflow — goes through exactly that sequence. A
`Deployment` would have been the dangerous shape: its default rolling update
surges one extra pod even at one replica.

Two things changed on the platform side anyway, because the invariant was
incidental and the failure was illegible:

- The manifest builder now states the no-surge strategy explicitly instead of
  inheriting the API server's default, with a test that fails if the tenant
  workload ever becomes a `Deployment` or grows a second replica.
- A refused boot is now reported as the refusal. The manager's startup gate
  polls `/healthz` and used to report only "did not become healthy before
  timeout", which reads the same for a lock refusal and a slow image pull. The
  tenant container runs with `terminationMessagePolicy:
  FallbackToLogsOnError`, so the message above reaches the pod status, and the
  manager reads it back into the error it returns.

What remains genuinely outside the rollout path — a pod force-deleted while
its node was unreachable, or a volume mounted by hand — is the case this lock
was written for, and it now surfaces as the message naming the directory.

## Running two hosts side by side

Give each its own root. The lock makes this mandatory rather than advisory: the
second host over one root is refused at boot, where it previously started and
wrote over the first's companies.

```sh
OPENCOMPANY_DATA_DIR=/tmp/oc-a opencompany serve \
  --company companies/e2e_harness --bind 127.0.0.1:8095 &
OPENCOMPANY_DATA_DIR=/tmp/oc-b opencompany serve \
  --company companies/e2e_harness --bind 127.0.0.1:8096 &
```

`--home /tmp/oc-a` places the bundles the same way and takes precedence, but it
does **not** move the shared workspace — prefer the variable for side-by-side
hosts.

## The desktop root is not the CLI root

The desktop shell does not use `$HOME/.opencompany`. It resolves the platform
application-data directory (`src-tauri/src/lib.rs::default_data_dir`) and passes
it explicitly to `app::prepare_instance`:

| OS      | Desktop root                                             |
| ------- | -------------------------------------------------------- |
| macOS   | `~/Library/Application Support/ai.tinyhumans.opencompany` |
| Windows | `%APPDATA%\ai.tinyhumans.opencompany`                     |
| Linux   | `$XDG_DATA_HOME/ai.tinyhumans.opencompany`, else `~/.local/share/…` |

`OPENCOMPANY_DATA_DIR` still wins where it is set, so pointing one build at a
scratch root — or at the CLI's root — is a single variable.

Passed explicitly rather than left to `resolve_home` because that resolver's
last branch is a *relative* `.opencompany`, which for a double-clicked
application resolves against whatever working directory the launcher supplied.

The consequence worth stating plainly: **a default desktop install and a default
`opencompany serve` are two separate instances.** Different companies, different
`instance-id`, no lock contention, nothing shared. That is deliberate — an
installed application's state belongs where the platform's backup and uninstall
tooling looks for it — but it surprises anyone who created a company in one and
went looking for it in the other.

The desktop takes the same prepare sequence as `serve` through
`app::boot::prepare_instance`: resolve, lock, migrate, materialize the
[workspace layout](workspace-layout.md), and probe the journal root. It does
**not** run `serve`'s storage-backend selection, so the desktop is always
`fs`-backed — `OPENCOMPANY_STORAGE`, `OPENCOMPANY_MONGODB_URI` and the sqlite
backend are command-line-only today.

## Instance identity

`<root>/instance-id` holds 16 random bytes, hex-encoded, minted on first boot
and served unauthenticated at `/spec`. It exists so a client holding several
connections can ask "is this the same host I already know?" and get a durable
answer — a URL cannot do that job, because a host moves between `localhost`, a
LAN address and a tunnel over one afternoon.

Random rather than derived from hostname, bind address or company set: `/spec`
is unauthenticated, so anything derived is a fact about the deployment handed to
anyone who asks. Random bytes name nothing. It authenticates nothing either —
do not grow a check that treats knowing it as proof of anything.

Both `instance-id` and `.lock` are runtime state and are ignored by git. A
committed `instance-id` would give every clone the same public identity, which
is the one thing the file exists to prevent.
