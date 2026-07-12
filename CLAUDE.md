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
> clippy/fmt, and the full manual checklist below all pass. Still **untested on
> macOS** specifically (filesystem-socket fallback path). The features added
> 2026-06-28 (multi-id `kill`/`priority`/`rerun` with range syntax; execution-order
> `list` sort + `--by-id` flag) are now **verified on both Linux and Windows**
> (all pure client-side logic, no `#[cfg]` branches). See the testing checklist.

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
`add [-l label] [-g N] [-p N] <argv...>`, `list [-a|--all] [-s|--state S]... [--by-id]`,
`info <id>`, `cat <id>`, `watch [<id>] [-a|--from-start]`, `kill <ids...> | kill --all`,
`priority <ids...> <n>`, `rerun <ids...> [-p N]`, `restart <ids...>`,
`pause [<ids...>]`, `resume [<ids...>]`, `remove <id>`, `clear`,
`config <setting>`, `shutdown`. The hidden `__daemon` subcommand runs the
background process and is not meant to be called directly.

`list` shows running, queued, and paused jobs by default; `--all` shows every
state and `--state <S>` (repeatable, mutually exclusive with `--all`) shows
exactly those states. The **default** active-jobs view is sorted by **execution
order** (running first, then queued jobs in priority order matching the scheduler,
then paused jobs — also in priority order, i.e. the order they would run once
resumed); `--by-id` reverts that view to id-order. The `--all` and `--state` views always use id-order
(chronological, natural for history), so `--by-id` is a no-op there. Filtering and
sorting are **client-side** (the daemon still returns all jobs), so the `List`
protocol is unchanged.

`kill`, `priority`, `rerun`, and `restart` all accept **multiple ids and ranges**: plain
numbers (`12 13 14`) or `start-end` ranges (`12-15` expands to 12, 13, 14, 15),
or a mix (`12 15-18`). Duplicates are silently deduplicated. Each id is processed
independently — errors are printed but processing continues for the remaining ids.
Client-side `parse_job_ids` expands the id args before sending individual
requests; no protocol change.

`rerun <ids...> [-p N]` re-queues each listed job as a brand-new job (new id, new
log), copying its argv, cwd, GPU request, label, and the environment captured at
the original `add` (`Request::Rerun { id, priority }` → `AppState::rerun`, which
clones those fields through `add`). The new job's **priority is not copied from
the source** — it defaults to `0`, or to `-p N` when given (the client always
sends a priority; clap's `default_value_t = 0` supplies the default). This lets a
rerun start at normal priority even if the original had been bumped. The source
job's record is left untouched; works for a job in any state.

`restart <ids...>` restarts a **running** job *in place* — same id and log file,
unlike `rerun` which makes a fresh job. `Request::Restart { id }` →
`AppState::request_restart`: for a running job it records the id in a transient
`restart_requested` set (`#[serde(skip)]`, not persisted) and the daemon fires
the job's kill switch. The process must fully die before the job can run again
(otherwise the scheduler could start a second instance / double-book its GPUs),
so the re-queue is deferred: the scheduler's run task now calls
`finish_or_restart` instead of `finish`, which — if the id is in the set —
resets the job to `Queued` (clears `started_at`/`finished_at`/`exit_code`/
`assigned_gpus`) rather than marking it terminal, then `notify` restarts it. The
next run's `File::create` truncates the log, so a restart starts with a clean
log. Setting the intent and consuming it both happen under the state `Mutex`, and
the intent is only ever set while the job is running, so the finish-vs-restart
choice is race-free. Non-running ids error (`NotRunning(state)`, with a `rerun`
hint) while processing continues for the rest, like `kill`.

`kill --all` cancels every *active* job at once: running jobs get their kill
switch fired (then become `Killed` via `finish`, like single `kill`) and queued
jobs are dropped immediately; terminal jobs are untouched (use `clear` to prune
those). Client maps `kill --all` to `Request::KillAll`; clap requires either
ids or `--all` and treats them as mutually exclusive.

`priority <ids...> <n>` re-prioritises **queued** jobs so they can jump the queue
(`Request::SetPriority`); it errors on running/terminal jobs
(`SetPriorityOutcome::NotQueued`). The last positional argument is always the
priority value; all preceding arguments are job ids (ranges accepted). `add -p N`
sets a job's priority at enqueue time.

`pause` / `resume` have two granularities selected by whether ids are given
(clap `ids: Vec<String>` with `num_args = 0..`; the client routes empty-vs-ids in
`pause_or_resume`):
- **No ids → all jobs.** `Request::PauseAllQueued` → `AppState::pause_all_queued`
  moves every `Queued` job to `Paused` (running/terminal untouched) and returns
  the count; the scheduler then starts nothing (only `Queued` is runnable), so
  running jobs finish but nothing new starts. `resume` (`ResumeAllPaused` →
  `resume_all_paused`) moves every `Paused` job back to `Queued` and notifies the
  scheduler. There is **no global paused flag** anymore — the state lives entirely
  in the jobs, so pausing then adding a fresh job lets that new job run. The
  no-id `resume` is **client-side guarded**: `resume_all` first fetches the job
  list; if any jobs are still `Queued` (i.e. not everything pending was paused) it
  prints a `[WARN]` and asks for y/N confirmation (`confirm`, defaults to no)
  before sending `ResumeAllPaused`, since a bare `resume` would otherwise un-pause
  more than intended. With nothing queued it resumes immediately; with no paused
  jobs it prints `(no paused jobs to resume)`.
- **With ids → individual jobs.** `PauseJob`/`ResumeJob` per id (ranges expanded
  client-side like `kill`). `pause_job` moves a **queued** job to the new
  `JobState::Paused` (errors on running/terminal via `PauseJobOutcome::NotQueued`;
  `AlreadyPaused` is a benign no-op); `resume_job` moves `Paused → Queued` and
  notifies. A paused job is skipped by the scheduler (only `Queued` is runnable),
  still shows in `list`, and is dropped by `kill`/`kill --all` like a queued job.

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
the log file, polling until the job is terminal. By **default it streams only
output produced from now on** — `follow` seeds its start offset with the log's
current size (`current_log_len`) so pre-existing history is skipped; `-a` /
`--from-start` seeds offset 0 to replay the whole log first. Ctrl+C (via
`tokio::signal::ctrl_c`) stops the watch but not the job — the job is the
daemon's child, not the client's, so the client never has a way to signal it.
It **only follows running jobs**: an explicit id that is queued/terminal prints
a `[WARN]` (with a `cat` hint for terminal jobs) and refuses to dump the
finished log; a missing id is an `[ERROR]`. The id is optional
(`Watch { id: Option<u32> }`) — with no id, `pick_running_job` auto-selects the
sole running job, or lists the running jobs (via `print_jobs`) when there are
several, or reports none. The id-less / list paths reuse `fetch_all_jobs` (a
`List` request), the same helper `list` now uses.

`info <id>` (`print_job_details`) shows an **elapsed** row computed client-side:
live (`Utc::now() - started_at`) while running, final (`finished_at - started_at`)
once terminal, `-` before it starts. `fmt_duration` renders only the significant
units (`5s`, `2m 3s`, `1h 2m 3s`, `1d 2h 3m 4s`).

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
  Killed, Failed}`, plus `Paused` (a queued job pulled aside by `msc pause <id>`;
  `Paused → Queued` on resume). `is_terminal()` (Finished/Killed/Failed only)
  drives `clear`, so paused jobs are kept. Job output (stdout+stderr merged) is
  redirected to a per-job log file `<log_dir>/<id>.log`; `cat` returns the path
  and the **client** reads the file directly.
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

### Testing against an isolated daemon (do not touch the user's real queue)

There is **one daemon per user** and the socket name is keyed on the
**username**, not `MOCHI_HOME` (`settings.rs` builds `mochi-<USER>.sock` from
`USER`/`USERNAME`). So `MOCHI_HOME` **alone does not isolate** the daemon: a
client with a fresh `MOCHI_HOME` still connects to the *existing* per-user
daemon over the same socket, and any test jobs land in the user's real queue.

To get a genuinely separate, throwaway daemon, override **both** env vars so the
socket *and* the data dir differ:

```bash
USERNAME=mochitest MOCHI_HOME=<scratch>/mochihome  msc add -- <test cmd>   # spawns an isolated daemon
USERNAME=mochitest MOCHI_HOME=<scratch>/mochihome  msc list
```

(On Unix set `USER` instead of `USERNAME`.) The client auto-spawns the isolated
daemon; it inherits these env vars, so its socket is `mochi-mochitest.sock` and
its state/logs live under the scratch dir — fully disjoint from the real queue.

**Always shut the isolated daemon down when finished**, or it lingers as a second
`msc __daemon` process:

```bash
USERNAME=mochitest MOCHI_HOME=<scratch>/mochihome  msc shutdown
```

Then verify only the user's real daemon remains (`Get-CimInstance Win32_Process
-Filter "Name='msc.exe'"` on Windows / `pgrep -af 'msc __daemon'` on Unix) — and
**never** force-kill an `msc __daemon` process, since you cannot tell the user's
real daemon from a test one by PID alone, and killing the real one disrupts their
queue.

## Testing checklist

Rows are individual behaviors; columns are the OSes they've been exercised on.
**Windows** is the primary development platform. **Linux** was verified on Arch
(kernel 7.0.12-arch1-1, rustc 1.96.0) on 2026-06-24 via an isolated `MOCHI_HOME`.
**macOS** is **still untested** — most importantly the filesystem-socket fallback
path (Linux uses the abstract-namespace socket `@mochi-<user>.sock`, Windows a
named pipe, macOS a filesystem socket).

Legend: ✅ verified (date, 2026 unless noted) · ➖ not yet tested · N/A not
applicable on this OS.

| Test | Win | Linux | macOS | Notes |
| --- | :---: | :---: | :---: | --- |
| Compiles & unit tests | ✅ | ✅ 06-24 | ➖ | `cargo build` + `cargo test` (36/36 green on Linux). |
| `clippy` / `fmt` clean | ✅ | ✅ 06-24 | ➖ | `cargo fmt --check` no diff; `clippy --all-targets` zero errors (only pre-existing dead-code warnings). |
| Daemon auto-spawn & detach | ✅ | ✅ 06-24 | ➖ | Client spawns the daemon; a job queued from a subshell that exits keeps running (Linux: daemon reparented to `systemd --user`). |
| Socket | ✅ named pipe | ✅ 06-24 abstract | ➖ | Per-user name (`mochi-<user>.sock`) avoids collisions. macOS filesystem-socket fallback is the key unverified path. |
| Basic lifecycle | ✅ | ✅ 06-24 | ➖ | `add` / `list` / `info` / `cat` / `watch` / `remove` / `clear` all behave as documented (watch tails live, stops once terminal). |
| `watch` running-only + optional id | ✅ 06-28 | ✅ 06-25 | ➖ | Queued → `[WARN]` (no `cat` hint); terminal → `[WARN]` + `msc cat <id>` hint; missing → `[ERROR]`. No id: auto-picks sole running job, lists via `print_jobs` when several, `(no running jobs to watch)` when idle. Live-tail + clean stop confirmed. |
| `rerun` (+ priority default) | ✅ 07-05 | ✅ 06-25 | ➖ | New queued job copies argv/label/cwd/GPU/env; source untouched (finished & running sources verified). Priority is **not** copied — defaults `0`, takes `-p N` (Win: `-p 20`→20, `-p -5`→-5, `rerun 428-429 -p 3`→whole range 3). |
| Whole-tree kill | ✅ Job Object | ✅ 06-24 pgroup | ➖ | `bash -c 'sleep 100 & sleep 100 & wait'` → one process group; `kill <id>` removed every process, no orphaned grandchildren. |
| `kill --all` | ✅ | ✅ 06-24 | ➖ | With `cpu-limit 1` forcing a running+queued mix: killed the running tree, dropped the queued; no orphans. |
| GPU isolation | ✅ | ✅ 06-24 | ➖ | `MOCHI_GPU_COUNT=2` → two `-g 1` jobs get `CUDA_VISIBLE_DEVICES=0`/`=1`; 0-GPU job hidden on Unix (empty var) but not isolated on Windows (documented diff). `MOCHI_GPU_VENDOR=amd` → HIP/ROCR vars. Over-request (`-g 5`) rejected. |
| Priority | ✅ | ✅ 06-24 | ➖ | `add -p N` and `priority <id> <n>` both reorder the queue (confirmed via `started` timestamps in `info`). |
| Env capture | ✅ | ✅ 06-24 | ➖ | Job picks up a variable exported only in the calling shell at `add` time (snapshot-based capture). |
| Daemon-crash cleanup gap | N/A | ✅ 06-24 | ➖ | Unix-only asymmetry: `kill -9` on the daemon orphans its child (no Job-Object kill-on-close); next start reconciles stale `Running` → `Failed`. On Windows the Job Object reaps the tree, so no gap exists. |
| Multi-id `kill` (ranges) | ✅ 06-28 | ✅ | ➖ | `kill 343 344-346 347 999999`: expands the range, processes each id by state (running → `Killed`, queued → `Dequeued`), `[ERROR]` on already-killed/missing while continuing, exits 1. |
| Multi-id `priority` (ranges) | ✅ 06-28 | ✅ | ➖ | `priority 348 349-351 999999 20`: trailing arg is the value, sets the queued range; errors on running (`not queued`)/missing while continuing, exits 1. |
| Multi-id `rerun` (ranges) | ✅ 06-28 | ✅ | ➖ | `rerun 343-345 347 999999`: re-queues each source as a new job, sources untouched, errors on missing while continuing, exits 1. |
| Execution-order `list` + `--by-id` | ✅ 06-28 | ✅ | ➖ | Default view: running first, then queued by priority desc / id asc; `--by-id` → pure id order. `--all` / `--state` always id (chronological) order regardless of `--by-id`. |
| `restart` (running, in place) | ✅ 07-12 | ➖ | ➖ | `restart 0` on a running counting job: same id, `started`/`elapsed` reset, log truncated back to line 1, keeps running. Errors on queued/terminal/missing (with a `rerun` hint) while continuing; multi-id + ranges like `kill`. GPUs released and reassigned on the re-run. |
| Bulk `pause` / `resume` (no id) | ✅ 07-12 | ➖ | ➖ | `pause` (no id) → every `queued` job becomes `paused` ("Paused N queued job(s)"); scheduler starts nothing new. `resume` (no id) with nothing else queued re-queues them all straight away; with other jobs still queued it prints a `[WARN]` and prompts y/N (`n` cancels, `y` resumes all). No global paused flag — pausing then adding a job lets the new job run. |
| Per-job `pause` / `resume` (ranges) | ✅ 07-07 | ➖ | ➖ | `pause 447-448` → `Paused`; scheduler **skips** them (killing the running job started nothing while both paused); `resume` → `Queued` and runs. Errors on running (pause)/non-paused (resume)/missing while continuing (exit 1). Shows in default `list`. |
| `watch` from-now default + `--from-start`/`-a` | ✅ 07-07 | ➖ | ➖ | Default watch on a job mid-output showed only new lines (LINE-5..8), while `cat` had all 8; `--from-start` and the `-a` alias replayed the whole log (LINE-1..8). |
| `info` elapsed | ✅ 07-07 | ➖ | ➖ | `elapsed` row: `-` while queued, live (`4s`) while running, fixed (`24s` = finished−started) once terminal. |

The multi-id and execution-order rows are pure client-side logic (no `#[cfg]`
branches), so Linux and Windows behave identically; their Windows runs (2026-06-28,
against the installed daemon with throwaway jobs and a temporary `cpu-limit 1`)
double as confirmation for both. The 2026-07-07 / 2026-07-12 rows (pause/resume,
watch from-now, info elapsed) are likewise `#[cfg]`-free — the per-job pause skip
lives in `take_next_runnable`, the bulk pause/resume-all in the daemon, the rest
is client-side — so they are expected to behave identically on Linux/macOS, but
have only been exercised on Windows so far. macOS remains the only fully
unverified target — fill in its column once a
machine is available.
