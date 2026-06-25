# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

`mochi_scheduler` is a cross-platform job queue. The binary is named `msc`. A CLI
client enqueues shell commands; a background daemon runs them sequentially (one at
a time, in id order) and persists state to disk so jobs survive across runs.

> **Platform status:** developed primarily on Windows; the Unix code paths
> (`#[cfg(unix)]`: `setsid`/`killpg` whole-tree kill in `process_tree.rs`,
> `setsid` daemon detach in `client.rs`, abstract/filesystem-socket fallback)
> have now been **verified on Linux (Arch, 2026-06-24)** — build, unit tests,
> clippy/fmt, and the full manual checklist below all pass. The `watch`
> running-only/optional-id change (2026-06-25) post-dates that pass and is so
> far **Windows-only tested** (client-side, no `#[cfg]`). Still **untested on
> macOS** specifically (filesystem-socket fallback path). See the testing
> checklist at the bottom of this file.

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
`add [-l label] [-g N] [-p N] <argv...>`, `list [-a|--all] [-s|--state S]...`,
`info <id>`, `cat <id>`, `watch [<id>]`, `kill <id> | kill --all`,
`priority <id> <n>`, `rerun <id>`, `remove <id>`, `clear`, `config <setting>`,
`shutdown`. The hidden `__daemon` subcommand runs the background process and is
not meant to be called directly.

`list` shows running and queued jobs by default; `--all` shows every state and
`--state <S>` (repeatable, mutually exclusive with `--all`) shows exactly those
states. Filtering is **client-side** (the daemon still returns all jobs), so the
`List` protocol is unchanged.

`rerun <id>` re-queues an existing job as a brand-new job (new id, new log),
copying its argv, cwd, GPU request, priority, label, and the environment captured
at the original `add` (`Request::Rerun` → `AppState::rerun`, which just clones
those fields through `add`). The source job's record is left untouched; works for
a job in any state.

`kill --all` cancels every *active* job at once: running jobs get their kill
switch fired (then become `Killed` via `finish`, like single `kill`) and queued
jobs are dropped immediately; terminal jobs are untouched (use `clear` to prune
those). Client maps `kill --all` to `Request::KillAll`; clap requires either an
id or `--all` and treats them as mutually exclusive.

`priority <id> <n>` re-prioritises a **queued** job so it can jump the queue
(`Request::SetPriority`); it errors on running/terminal jobs
(`SetPriorityOutcome::NotQueued`). `add -p N` sets a job's priority at enqueue
time.

Daemon settings live under `msc config <setting>` (a nested `clap` subcommand,
`ConfigCommand` in `cli.rs`) so they share one namespace and `--help` lists them
together; add new settings as `ConfigCommand` variants. Currently:

- `config cpu-limit [N]` gets (no arg) or sets the max number of concurrent CPU
  (0-GPU) jobs; `0` means unlimited (the default). The cap lives in
  `AppState.cpu_limit` (persisted, `serde(default)` None) and is enforced in
  `take_next_runnable`: a 0-GPU job is runnable only while
  `running_cpu_count() < cpu_limit`. GPU jobs are bounded by the GPU pool and
  ignore it. Client maps the arg (`0` → unlimited) to `SetCpuLimit`/`GetCpuLimit`;
  setting it re-notifies the scheduler.

`watch` is client-side only: it reuses `Info` (for log path + state) and tails
the log file, polling until the job is terminal. Ctrl+C (via
`tokio::signal::ctrl_c`) stops the watch but not the job — the job is the
daemon's child, not the client's, so the client never has a way to signal it.
It **only follows running jobs**: an explicit id that is queued/terminal prints
a `[WARN]` (with a `cat` hint for terminal jobs) and refuses to dump the
finished log; a missing id is an `[ERROR]`. The id is optional
(`Watch { id: Option<u32> }`) — with no id, `pick_running_job` auto-selects the
sole running job, or lists the running jobs (via `print_jobs`) when there are
several, or reports none. The id-less / list paths reuse `fetch_all_jobs` (a
`List` request), the same helper `list` now uses.

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
  `tokio::select!` in `run_one` uses to terminate the job.
- **Whole-tree kill (`process_tree.rs`):** a job command is usually a *chain*
  (`pixi run ... python`), and the process holding the RAM/GPU is a grandchild.
  Killing only the direct child orphans it, so `run_one` ties each job to an OS
  primitive that kills the whole tree: on Windows a **Job Object** with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (assigned via `process_tree::Guard::attach`
  right after spawn; `kill` calls `TerminateJobObject`, and the kill-on-close flag
  also reaps jobs if the daemon itself dies); on Unix a **process group** (the
  child gets its own session via `setsid` in `process_tree::configure`'s
  `pre_exec`, and `kill` sends `SIGKILL` via `killpg`). `kill` here used to be
  `child.start_kill()`, which only terminated the direct child — the bug that
  left python workers running. Deps: `windows-sys` (Job Objects) on Windows,
  `libc` (setsid/killpg) on Unix, both target-gated in `Cargo.toml`.
- **GPU scheduling (`gpu.rs` + `scheduler.rs`):** Each GPU is a resource. A job
  declares a count (`msc add --gpus N`); `AppState::take_next_runnable(total)`
  scans queued jobs and, among those that currently fit the free pool, starts the
  one with the highest `Job::priority` (ties broken by lowest id). A job that
  doesn't fit is skipped rather than blocking the queue (**greedy backfill** — a
  0-GPU job is always runnable, a small/higher-priority job can run ahead of a
  blocked larger one), reserving the lowest free indices in `Job::assigned_gpus`.
  Priority is set at enqueue (`add -p N`, default 0) or changed for a queued job
  with `priority <id> <n>`; higher runs first. `run_one` isolates the child by exporting the vendor's
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

## Linux testing checklist

Verified on Arch Linux (kernel 7.0.12-arch1-1, rustc 1.96.0) on 2026-06-24, all
via an isolated `MOCHI_HOME`. macOS is still untested — in particular the
filesystem-socket fallback path (Linux uses the abstract-namespace socket,
confirmed below as `@mochi-<user>.sock`).

- [x] **Compiles & unit tests:** `cargo build` and `cargo test` pass on Linux
      (36/36 unit tests green).
- [x] **`clippy` / `fmt`** clean on Linux (`cargo fmt --check` no diff;
      `cargo clippy --all-targets` zero errors, only pre-existing dead-code
      warnings plus one new one: `process_tree.rs`'s `use
      std::os::unix::process::CommandExt` is unused because `tokio::process::Command`
      already exposes `pre_exec` itself on Unix — harmless, worth a follow-up
      cleanup).
- [x] **Daemon auto-spawn & detach:** a client call spawns the daemon; a job
      queued from a subshell that immediately exits keeps running and the
      daemon is reparented to `systemd --user` (confirmed via `ps`), i.e. fully
      detached from the spawning shell.
- [x] **Socket:** abstract-namespace socket `@mochi-<user>.sock` confirmed via
      `/proc/net/unix`; per-user naming avoids collisions. (macOS
      filesystem-socket fallback still unverified — no macOS box available.)
- [x] **Basic lifecycle:** `add` / `list` / `info` / `cat` / `watch` / `remove`
      / `clear` all behave as documented (watch correctly tails the log live and
      stops once terminal).
- [x] **`watch` running-only + optional id (verified 2026-06-25):** `watch <id>`
      on a queued job prints `[WARN]` and refuses; on a terminal job (finished or
      killed) prints `[WARN]` plus a `msc cat <id>` hint; a nonexistent id prints
      `[ERROR]`. With no id: auto-picks the sole running job and tails it; lists
      running jobs (via `print_jobs`) when there are several; prints `(no running
      jobs to watch)` when idle. All six cases confirmed on Linux (pure
      client-side logic, tested against the installed daemon).
- [x] **`rerun` (verified 2026-06-25):** `rerun <id>` creates a new queued job
      copying argv, label, and priority from the source; source job record is
      left untouched. Verified for a finished job and for a running job.
- [x] **Whole-tree kill:** a job running `bash -c 'sleep 100 & sleep 100 & wait'`
      put all 3 processes in one process group (confirmed via `ps
      -eo pid,ppid,pgid`); `kill <id>` removed every one (`pgrep -g <pgid>`
      empty afterward) — no orphaned grandchildren.
- [x] **`kill --all`:** with `cpu-limit 1` forcing a running+queued mix, `kill
      --all` killed the running job's whole tree and dropped the queued ones;
      no orphan processes remained.
- [x] **GPU isolation:** with `MOCHI_GPU_COUNT=2`, two `-g 1` jobs got
      `CUDA_VISIBLE_DEVICES=0` and `=1` respectively; a 0-GPU job got an empty
      value (correctly hidden from all GPUs, the documented Unix-vs-Windows
      difference). `MOCHI_GPU_VENDOR=amd` also verified: jobs see
      `HIP_VISIBLE_DEVICES`/`ROCR_VISIBLE_DEVICES` instead. Over-requesting GPUs
      (`-g 5` with only 2 available) is rejected as expected.
- [x] **Priority:** `add -p N` and `priority <id> <n>` both reorder the queue;
      verified a job promoted via `priority` ran before a higher-id job that had
      a lower priority (confirmed by `started` timestamps in `info`).
- [x] **Env capture:** a job picked up a variable exported only in the calling
      shell at `add` time, confirming the snapshot-based env capture works on
      Unix.
- [x] **Daemon-crash cleanup gap (confirmed, by design):** `kill -9` on the
      daemon left its running child (`sleep 60`) orphaned — Unix process groups
      have no kill-on-close equivalent to Windows Job Objects. On the next
      daemon start, `AppState::load`'s reconciliation correctly flipped the
      stale `Running` job to `Failed`. This matches the documented asymmetry; no
      action taken (no shutdown-time `killpg` sweep added), since `shutdown` is
      a clean exit path and crashes are rare/already reconciled on next start.
