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
cargo install --path .      # install `msc` into ~/.cargo/bin (already on PATH); re-run to update
```

Edition is **2024** (requires a recent toolchain).

### Iterating after a code change

The daemon is **long-lived** and keeps running the binary it was first spawned
from — it does not pick up rebuilds on its own. Always restart it after building:

```bash
cargo run -- shutdown   # stop the old daemon (or it keeps serving stale logic)
cargo build             # IMPORTANT: a running daemon locks msc.exe on Windows,
                        # so skipping the shutdown makes the rebuild fail silently
cargo run -- <args>     # next client auto-spawns a fresh daemon
```

Symptoms of a stale daemon: new flags (e.g. `-g`) appear swallowed into the
job's `argv`, or behavior doesn't match the current source. Note there is **one
daemon per user** (the socket name is keyed on the username, not `MOCHI_HOME`),
so `MOCHI_HOME` alone won't spin up a second, isolated daemon — shut the running
one down first.

### CLI subcommands (`msc <cmd>`)
`add [-l label] [-g N] <argv...>`, `list`, `info <id>`, `cat <id>`,
`watch <id>`, `kill <id>`, `remove <id>`, `clear`, `cpu-limit [N]`,
`shutdown`. The hidden `__daemon` subcommand runs the background process and is
not meant to be called directly.

`cpu-limit [N]` gets (no arg) or sets the max number of concurrent CPU (0-GPU)
jobs; `0` means unlimited (the default). The cap lives in `AppState.cpu_limit`
(persisted, `serde(default)` None) and is enforced in `take_next_runnable`: a
0-GPU job is runnable only while `running_cpu_count() < cpu_limit`. GPU jobs are
bounded by the GPU pool and ignore it. Client maps the arg (`0` → unlimited) to
`SetCpuLimit`/`GetCpuLimit`; setting it re-notifies the scheduler.

`watch` is client-side only: it reuses `Info` (for log path + state) and tails
the log file, polling until the job is terminal. Ctrl+C (via
`tokio::signal::ctrl_c`) stops the watch but not the job — the job is the
daemon's child, not the client's, so the client never has a way to signal it.

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
  `tokio::Notify`. A separate `scheduler` task waits on that `Notify`, then starts
  every job that fits the free GPU pool, each in its own `tokio::spawn` (jobs run
  **concurrently**, not one at a time). A finishing job releases its GPUs and
  re-notifies so the scheduler backfills. Running jobs are tracked in a
  `kills: HashMap<id, oneshot::Sender>`; `kill` fires the oneshot, which a
  `tokio::select!` in `run_one` uses to terminate the child.
- **GPU scheduling (`gpu.rs` + `scheduler.rs`):** Each GPU is a resource. A job
  declares a count (`msc add --gpus N`); `AppState::take_next_runnable(total)`
  scans queued jobs in id order and starts the first whose need fits the free
  pool (**greedy backfill** — a 0-GPU job is always runnable, a small job can run
  ahead of a blocked larger one), reserving the lowest free indices in
  `Job::assigned_gpus`. `run_one` isolates the child by exporting the vendor's
  visible-devices var(s) (`CUDA_VISIBLE_DEVICES` for NVIDIA;
  `HIP_VISIBLE_DEVICES`+`ROCR_VISIBLE_DEVICES` for AMD). GPU count/vendor are
  detected once at daemon startup via `gpu::detect()` (`nvidia-smi -L` /
  `rocm-smi`), overridable with `MOCHI_GPU_COUNT` (+ optional `MOCHI_GPU_VENDOR`)
  — the supported way to test without real hardware. Adds requesting more GPUs
  than exist are rejected. **Caveats:** (1) because 0-GPU jobs are always
  runnable, plain (non-GPU) jobs now also run concurrently rather than serially;
  (2) a 0-GPU job is hidden from all GPUs on Unix (empty visible-devices var),
  but on Windows the empty value can't be set so it isn't GPU-isolated.
- **State (`ipc/scheduler.rs::AppState`):** `BTreeMap<u32, Job>` + `next_id`,
  serialized to `state.json`. Persisted via **write-temp-then-rename** so a crash
  never leaves a half-written file. On `load`, any job still marked `Running`
  (daemon died mid-run) is reconciled to `Failed`.
- **Job model (`ipc/job.rs`):** `JobState` is `Queued → Running → {Finished,
  Killed, Failed}`. `is_terminal()` drives `clear`. Job output (stdout+stderr
  merged) is redirected to a per-job log file `<log_dir>/<id>.log`; `cat` returns
  the path and the **client** reads the file directly.
- **Working dir & environment capture:** the client snapshots its `cwd` and full
  environment (`std::env::vars()`) at `add` time and sends them in the request;
  the job is persisted with both and `run_one` applies them (`env_clear` then the
  snapshot, GPU vars layered on top). This is what makes a job run with the
  caller's active shell env — e.g. a `pixi run nu` / venv / conda activation —
  rather than the daemon's frozen one. Legacy jobs with an empty snapshot fall
  back to inheriting the daemon env.
- **Settings (`settings.rs`):** Resolves all paths under one data dir
  (`directories` crate, or `MOCHI_HOME` override — useful for tests/isolated
  queues). Socket name is **per-user** (`mochi-<user>.sock`) to avoid collisions
  in the global Windows named-pipe namespace. Namespaced sockets are used where
  supported, falling back to a filesystem socket path (e.g. macOS).
- **`utils/pretty_table`:** Hand-rolled table renderer (frame/header/text styles,
  colored output) used by the client to print `list` and `info`. It replaced
  `comfy_table` — leftover commented-out preset calls in `client.rs` are from that
  migration. `fit_to_width(width, col)` caps the table at a width and truncates
  that column's cells with `...`; `list` passes the detected terminal width
  (`terminal_size`) and the command column, and skips truncation when output
  isn't a terminal (piped/redirected).
- **No console window for jobs (Windows):** `run_one` sets `CREATE_NO_WINDOW` on
  the child so a console job spawned by the console-less daemon doesn't pop up a
  window; its stdio is already redirected to the log file.

## Conventions

- The daemon currently prints received requests to stdout (`handle_conn`) — debug
  noise, not a feature.
- `MOCHI_HOME` is the intended mechanism for test isolation: point it at a temp
  dir to get a fresh queue and socket.
