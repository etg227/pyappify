# Tool Hub development preview

This fork of [ok-oldking/pyappify](https://github.com/ok-oldking/pyappify) adds a
multi-application desktop entry point. Upstream source and GPLv3 license are
retained. The legacy single-application services are retained while the new
engine is introduced independently; they are not used for hub applications.

## Available now

- A catalog containing uninstalled as well as installed tools.
- Application IDs, operation locks, installation state, logs and processes are
  isolated per tool. Different tools can run at the same time.
- Corrupted state in one tool is reported on that card without hiding other tools.
- Branch-based update checks resolve and display a full commit SHA. Installation
  uses the checked SHA, even if the remote branch changes before confirmation.
- Source and Python environments use separate immutable deployment directories.
  The environment is created at its final location, never renamed after creation.
- Persistent data is outside deployments. Dependency preparation failures retain
  the current deployment; failure to become ready after process exit restores the
  previous deployment when available. Manual restore swaps source and environment.
- Stop uses an application lifecycle protocol, waits for cleanup, and does not
  indiscriminately kill Python processes. Updating a running tool is rejected.
- Closing the main window hides it; explicit exit stops managed tools first.

## Preview limitations

The catalog explicitly tracks FLY's `feat/tool-hub-runtime` development branch.
This is not a stable channel. Automatic runtime downloads, CI-qualified stable
commit selection, remote catalog refresh and a signed/distributed installer are
not provided by this iteration. Do not describe it as a finished public release.

Installation requires an existing Python executable. FLY currently requests
Python 3.12 with Tcl/Tk. Installation validates this requirement, creates an
isolated venv and checks it again. FLY itself uses the standard library. Tools
with third-party dependencies can declare a requirements file, installed with
`--require-hashes`; generating those lock files belongs to each tool repository.

The engine automatically restores an earlier deployment only when a candidate
process exits before readiness. A hung process remains tracked, with an explicit
error, rather than being force-killed. After a manager restart, a matching session
can receive stop/show commands; an ambiguous or stale session blocks installation
until investigated. Full crash recovery and a safe repair UI remain follow-up work.

There is no automatic cleanup of old deployments in this preview, and startup logs
are append-only. The log viewer reads at most the last 64 KiB. User data migrations
cannot be reversed merely by restoring code: tools must supply their own compatible
migrations and backups. Old standalone FLY installs are not automatically imported.

## Code boundaries

```text
catalog/catalog.json       bundled list of tools and repository refs
crates/hub-core/           catalog, repository, deployment and process engine
src-tauri/src/hub.rs       thin Tauri command adapter
src/hub/                   multi-tool frontend
src/App.tsx               retained upstream single-tool UI
```

`?mode=legacy` selects the retained single-tool frontend for comparison. It is
loaded lazily and is not part of the hub's initial interface.

## Run and verify

On Windows with Rust stable, Node 24+, pnpm 11 and Python 3.12:

```text
pnpm install --frozen-lockfile
pnpm tauri dev
```

Click FLY's install action, review the candidate commit and provide the path to
Python 3.12. Once installed, start FLY, open its window or logs, stop it normally,
and check for updates. The Windows preview workflow builds an executable artifact
without publishing a Release. The executable's file name remains `pyappify.exe`
for compatibility with upstream build configuration; its application identity and
data directory are distinct from PyAppify.

```text
pnpm build
cargo test --manifest-path crates/hub-core/Cargo.toml --locked
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib
```

Core integration tests create actual Git repositories, Python venvs and two
concurrent managed child processes. They cover fixed commit installation,
data preservation, source/environment restore, failed environment preparation,
candidate startup failure, documentation-only changes and rewritten branch history.
Set TEST_PYTHON to select the test interpreter if it is not available as `python`
on Windows or `python3` elsewhere.

## Add a tool

Add a stable lowercase ID in the catalog, with description, repository, branch,
platforms and channel=preview. The selected source commit must contain:

```json
{
  "schema": 1,
  "id": "example",
  "python": "3.12",
  "script": "main.py",
  "requires_tk": false,
  "lifecycle": "file-v1"
}
```

The script accepts `--data-dir` and `--hub-session`. See FLY's integration document
for the readiness and stop/show file protocol. Runtime state belongs in the given
data directory, not the source directory. Display-name changes must not change ID.

## Next milestones

1. Validate actual FLY GUI, Tkinter, proxy restoration and UAC on clean Windows.
2. Provide a verified managed Python runtime including Tcl/Tk and a download UI.
3. Add CI-qualified commit targets and remote catalog refresh before a public channel.
4. Add recovery journals, stale-session repair, log rotation and deployment cleanup.
5. Complete FLY's internal package reorganization without duplicating update engines.
