# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

`mochi_scheduler` is a cross-platform job queue. The binary is named `msc`. A CLI
client enqueues shell commands; a background daemon runs them sequentially (one at
a time, in id order) and persists state to disk so jobs survive across runs.

## Commands

```bash
cargo build                 # build the `msc` binary
cargo run -- <args>         # run the client, e.g. `cargo run -- add cargo build`
cargo run -- __daemon       # run the daemon in the foreground (debugging only)
cargo test                  # run tests
cargo test <name>           # run a single test by name substring
cargo fmt                   # format (config in rustfmt.toml: max_width 110, crate-granularity imports)
cargo clippy
```

Edition is **2024** (requires a recent toolchain).

### CLI subcommands (`msc <cmd>`)
`add [-l label] <argv...>`, `list`, `info <id>`, `cat <id>`, `kill <id>`,
`remove <id>`, `clear`, `shutdown`. The hidden `__daemon` subcommand runs the
background process and is not meant to be called directly.

## Architecture

Client and daemon are the **same binary**, dispatched in `main.rs`: `__daemon`
runs `daemon::run`, everything else runs `client::run`.

- **Auto-spawn:** The client connects to the daemon's socket; if nothing is
  listening it spawns `msc __daemon` as a detached background process
  (`client.rs::spawn_daemon`, platform-specific via `creation_flags` on Windows /
  `setsid` on Unix) and retries the connection for ~5s. Users never start the
  daemon manually.
- **Transport:** A single request/response per connection over a local socket
  (`interprocess` crate). `protocol.rs` defines the `Request`/`Response` enums and
  length-prefixed JSON framing (`write_msg`/`read_msg`, 16 MiB cap).
- **Daemon (`ipc/daemon.rs`):** Accepts connections, each handled in its own
  task. `handle_request` mutates shared state under a `Mutex` and pokes a
  `tokio::Notify`. A separate `scheduler` task waits on that `Notify`, then drains
  all queued jobs one at a time via `run_one`. Running jobs are tracked in a
  `kills: HashMap<id, oneshot::Sender>`; `kill` fires the oneshot, which a
  `tokio::select!` in `run_one` uses to terminate the child.
- **State (`ipc/scheduler.rs::AppState`):** `BTreeMap<u32, Job>` + `next_id`,
  serialized to `state.json`. Persisted via **write-temp-then-rename** so a crash
  never leaves a half-written file. On `load`, any job still marked `Running`
  (daemon died mid-run) is reconciled to `Failed`.
- **Job model (`ipc/job.rs`):** `JobState` is `Queued → Running → {Finished,
  Killed, Failed}`. `is_terminal()` drives `clear`. Job output (stdout+stderr
  merged) is redirected to a per-job log file `<log_dir>/<id>.log`; `cat` returns
  the path and the **client** reads the file directly.
- **Settings (`settings.rs`):** Resolves all paths under one data dir
  (`directories` crate, or `MOCHI_HOME` override — useful for tests/isolated
  queues). Socket name is **per-user** (`mochi-<user>.sock`) to avoid collisions
  in the global Windows named-pipe namespace. Namespaced sockets are used where
  supported, falling back to a filesystem socket path (e.g. macOS).
- **`utils/pretty_table`:** Hand-rolled table renderer (frame/header/text styles,
  colored output) used by the client to print `list` and `info`. It replaced
  `comfy_table` — leftover commented-out preset calls in `client.rs` are from that
  migration.

## Conventions

- The daemon currently prints received requests to stdout (`handle_conn`) — debug
  noise, not a feature.
- `MOCHI_HOME` is the intended mechanism for test isolation: point it at a temp
  dir to get a fresh queue and socket.
