# Local instances

The desktop shell runs a roster of hosts on one machine. This file covers the
roster, what an empty data root means for onboarding, and how to run the shell
in development. The shell's other halves — the proxy, the keychain, the
embedded host itself — are in [`desktop.md`](desktop.md).

## Several hosts on one machine

`src-tauri/src/local.rs` is the layer above `embedded.rs`: `embedded` starts
*one* host over *one* data root and says nothing about which roots exist;
`LocalHosts` is the roster of the roots an operator asked for, and which of them
are listening. Two hosts cannot share a root (`prepare_instance` locks it,
because two would overwrite each other's companies), so a second local company
is a **second root**, not a second process over the first.

The roster is `<data-dir>/instances.json`:

```json
{
  "instances": [
    { "id": "default", "label": "This computer", "autostart": true },
    { "id": "acme", "label": "Acme", "root": "instances/acme", "autostart": true }
  ]
}
```

- **`id`** is minted from the label and is the directory name under
  `instances/`. Renaming changes the label and never the id, so the data stays.
- **`root` absent means the data dir itself** — the `default` instance, where
  every install predating this file already keeps its company. Moving it under
  `instances/default/` would be a migration whose failure mode is "my company is
  gone", in exchange for symmetry.
- **`autostart`** records the last explicit start or stop, so a stopped instance
  is not silently restarted by the next launch.

A `root` that escapes the data dir — absolute, or containing `..` — is dropped
when the roster is read: it is a plain file an operator can edit, and a
hand-edit must not point a host and its lock somewhere this application never
chose.

Commands: `oc_local_instances` lists, `oc_create_local_instance` adds a root and
starts it, `oc_start_local_instance` / `oc_stop_local_instance` take and release
one, `oc_rename_local_instance` changes only the label, and
`oc_forget_local_instance` drops a row **leaving its data on disk** — the
reversible half only, because the other half is someone's company. Stopping
frees the root for an `opencompany serve` in a terminal.

An instance that fails to start is a row carrying its reason, never a launch
that fails: one busy root must not stop the other instances, or the multi-host
case would be worse than the single-host one it replaces.

### Which empty root gets a company, and which gets the wizard

The two hosts differ in exactly one decision, `embedded::FirstRun`:

- **The instance at the data root** seeds a starter company from the default
  preset (`SeedStarterCompany`). That is [issue #632](https://github.com/tinyhumansai/opencompany/issues/632):
  a double-clicked application must be enterable with no terminal and no
  decisions.
- **An instance an operator created** does not (`RunSetupWizard`). They are
  standing in front of the application having just named a company, so the
  decisions the first-run wizard asks for — template, sign-in mode, brain
  credential — are ones they are already making.

Seeding is not merely "adds a company". `AppSpec` reports
`setup_complete: stamp || !registry.is_empty()`, and the console opens
`views/setup/SetupWizard.tsx` only on `setup_complete: false` — so a seeded
company **suppresses the wizard permanently**. Both halves are pinned by a test
that reads `/spec` over HTTP rather than counting companies.

`RunSetupWizard` skips the *seed* half of `bootstrap_companies` and keeps the
*adopt* half (`desktop::adopt_companies`). Adoption is not optional for either
host: a company the wizard writes is a bundle on disk, and skipping adoption
would mean an instance that came back from every relaunch serving an empty
registry — and reporting setup outstanding again, once per launch.

`oc_embedded` survives as the `default` instance's row, because the shell and
the console ship independently — a `pnpm dev` console against an older `cargo`
build, or a current console against an older shell, degrades to the one-host
behaviour instead of to an unhandled command.

### First run

On the `SeedStarterCompany` branch — `embedded::start`, which is the wrapper
`start_with` exposes for it — `opencompany::desktop::bootstrap_companies` runs
before the bind, because a host with an empty registry cannot be signed into
([issue #632](https://github.com/tinyhumansai/opencompany/issues/632)). Sign-in
is per-company — `/api/v1/companies/{id}/auth/…`, or the sole-company alias —
so an empty registry leaves the console rendering a login form for a company
that does not exist.

That is why a *created* instance is not left at an empty registry either: it
takes the `RunSetupWizard` branch, where the wizard is what puts a company
there. The rest of this section describes the seeding branch.

The two ways a company normally reaches the registry are both closed to a
packaged application. Nobody types `serve --company <dir>` at a double-clicked
app, and `POST /api/v1/companies` demands the `platform` scope, which
`PlatformScope` grants only against a configured `platform_auth` — a prosumer
host has no machine credential to hand out, deliberately. So the desktop
bootstraps its own:

1. **Adopt** every company bundle the data root already holds, skipping
   `archived` ones (archiving removes a company from the registry on purpose,
   and re-registering it at the next launch would undo that quietly). The
   bundle is the only authority — a desktop company has no source directory to
   re-read — and `RuntimeBuilder::build` carries the persisted record's
   console-created desks, agents and workflows forward.
2. **Seed** the `DEFAULT_PRESET_ID` preset when there were none, stamping the
   preset slug as the record's template provenance. Fallback rather than
   unconditional: seeding on every launch would hand the operator a second
   starter company per run.

`AppConfig.admin_email` is set to `DESKTOP_OPERATOR_EMAIL` — the same seam the
hosted control plane fills with `OPENCOMPANY_ADMIN_EMAIL`, and the reason a
person is eligible to sign in at all (`eligibility` in `src/server/users/`
admits an existing user, a bootstrap admin, or an invite, and a fresh install
has none of the three). The seeded manifest names the same address in its own
`[users].admins`, so the company is self-describing if it is ever served
elsewhere.

Nothing is mailed: the host binds loopback, so `is_local_only` holds and
`auth/request` returns the login code in its own response (`dev_code`), which
the console redeems in place. `oc_embedded` carries `operatorEmail` so the
sign-in form can offer the address — a person cannot guess it, and every other
address gets the same silent `202`. It is a suggestion, not a lock: the field
stays editable, which is what an operator who invites someone else needs.

## Running the shell in development

A debug build loads `devUrl` rather than the embedded bundle, so without a
console dev server the window is blank. Use:

```bash
OPENCOMPANY_DATA_DIR=$PWD/target/desktop-dev ./scripts/desktop-dev.sh
```

It starts the dev server (reusing one already on `:5173`), waits for it to
answer, and runs the shell from `src-tauri/`.

**Not** `build.beforeDevCommand`. The Tauri CLI runs that hook from a directory
it *derives* by scanning for a `package.json`, and which one it picks is not
stable — on a macOS checkout it lands in `frontend/`, on CI's runner it landed
in `vendor/openhuman/`. No relative path is correct from both, so both hooks
are deliberately empty and `ci.yml` packages from two different working
directories to keep them that way (issue #616). A script can do what the hook
cannot: derive every path from its own location.
