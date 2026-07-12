# mochi_scheduler (`msc`)

A small cross-platform job queue. Hand a command to `msc` and a background
daemon runs it for you, capturing its output and tracking its state. Jobs can
reserve GPUs, so GPU-bound work runs concurrently up to the number of GPUs on
the machine and queues when the pool is full.

- **No server to manage** — the first command auto-spawns a background daemon;
  it keeps running and survives your shell closing.
- **GPU-aware** — declare how many GPUs a job needs; the scheduler reserves
  them, isolates each job to its assigned devices, and queues the rest.
- **Faithful environment** — a job runs with the working directory *and*
  environment you had when you queued it (e.g. a `pixi` / venv / conda
  activation), not some stale daemon state.
- **Durable** — the queue is persisted to disk and reloads across restarts.

## Install

```bash
cargo install --path .
```

This builds a release binary and installs `msc` into `~/.cargo/bin`, which is
already on your `PATH` from rustup. Re-run it after pulling changes to update
the installed binary.

## Quick start

```bash
msc add cargo build --release     # queue a command (everything after `add` is the command)
msc add -l nightly -- ./train.sh  # give it a label; use `--` before flags meant for the command
msc add -p 10 -- ./urgent.sh      # queue at higher priority so it runs before normal jobs
msc list                          # running + queued jobs (use --all or --state for others)
msc info 3                        # full details for job 3
msc cat 3                         # print job 3's captured output (stdout + stderr)
msc watch 3                       # follow a job's output live from now on; Ctrl+C stops watching, not the job
msc watch 3 --from-start          # ...replaying the whole log so far first
msc watch                         # watch the sole running job, or list the running jobs to choose from
msc priority 3 10                 # bump a queued job's priority so it jumps the queue
msc rerun 3                       # re-queue a fresh copy of job 3 (same command, dir, env) at priority 0
msc rerun 3 -p 10                 # re-queue job 3 at a chosen priority instead of the default 0
msc restart 3                     # restart a running job in place: kill it and re-run the same job
msc pause                         # pause every queued job; running ones finish, nothing new starts
msc resume                        # put all paused jobs back into the queue
msc pause 4 5-7                   # pull specific queued jobs out of the queue until resumed
msc resume 4 5-7                  # put those paused jobs back into the queue
msc kill 3                        # stop a running job, or drop a queued one
msc kill --all                    # stop every running job and drop every queued one
msc remove 3                      # remove a finished job from the list
msc clear                         # remove all finished/killed/failed jobs
msc config cpu-limit 4            # cap concurrent CPU (non-GPU) jobs at 4 (0 = unlimited)
msc shutdown                      # stop the background daemon
```

The daemon starts automatically the first time you run any command, so you never
launch it by hand.

## GPU scheduling

Each GPU on the host is a reservable resource. Declare how many a job needs with
`-g` / `--gpus`:

```bash
msc add -g 1 -- python train.py        # reserve 1 GPU
msc add -g 2 -- python train_big.py    # reserve 2 GPUs
msc add -- python prep.py              # no GPUs (default)
```

How it schedules:

- Jobs run **concurrently** as long as enough GPUs are free. A job that needs
  more GPUs than are currently available waits in the queue; when a running job
  finishes and releases its GPUs, a waiting job takes them.
- Each job is **isolated** to exactly the GPUs it was assigned via the vendor's
  visible-devices variables (`CUDA_VISIBLE_DEVICES` for NVIDIA;
  `HIP_VISIBLE_DEVICES` + `ROCR_VISIBLE_DEVICES` for AMD).
- `msc list` / `msc info` show each job's GPU request and, while running, the
  assigned indices (e.g. `[0,2]`).
- A request for more GPUs than the machine has is rejected immediately.

GPU count and vendor are detected once at daemon startup via `nvidia-smi` /
`rocm-smi`. You can override detection with `MOCHI_GPU_COUNT` (and optionally
`MOCHI_GPU_VENDOR`), which is also the way to exercise GPU scheduling on a
machine without real hardware:

```bash
MOCHI_GPU_COUNT=2 msc add -g 1 -- ./job.sh
```

> Note: because a 0-GPU job always fits, plain (non-GPU) jobs also run
> concurrently rather than one at a time.

### Priority (jumping the queue)

Every job has a scheduling priority (default `0`). When capacity frees up, the
scheduler starts the highest-priority job that fits; ties break by id (oldest
first), so equal-priority jobs keep FIFO order.

```bash
msc add -p 10 -- ./urgent.sh   # enqueue ahead of normal (priority 0) jobs
msc priority 7 5               # bump an already-queued job so it runs sooner
msc priority 7 -1             # or push one to the back with a lower priority
```

`priority` only affects **queued** jobs — a job that has already started keeps
running. Priority changes the *order* jobs start in, not whether a job fits: a
high-priority GPU job still waits until enough GPUs are free.

### Cancelling everything

`kill <id>` stops one job; `kill --all` cancels every active job at once —
running jobs are stopped and queued jobs are dropped. Finished/killed/failed
jobs are left in the list (use `clear` to prune those).

```bash
msc kill --all   # stop all running jobs and clear the queue
```

### Restarting a running job

`restart` stops a running job and immediately runs it again as the **same job**:

```bash
msc restart 3        # kill job 3's process tree and re-run it from the start
msc restart 3 5-7    # multiple ids and ranges, like kill
```

The job keeps its id, priority, and log file (the log is truncated for the fresh
run). Only **running** jobs can be restarted — for a finished job use `rerun`,
which instead queues a brand-new copy (new id, new log). If a listed job isn't
running, `restart` reports an error and moves on to the rest.

### Pausing

`pause` holds work back without cancelling it, in two granularities:

```bash
msc pause        # pause every queued job at once
msc resume       # put all paused jobs back into the queue
msc pause 4 5-7  # pause specific queued jobs (ranges accepted)
msc resume 4 5-7 # resume them
```

- **All jobs** (`pause` with no id): every currently **queued** job is moved to
  the `paused` state, so nothing new starts (jobs already running finish
  normally). `resume` with no id puts them all back. If some jobs are still
  queued when you `resume` (i.e. not everything pending is paused), a bare
  `resume` would un-pause more than you may have intended, so it prints a warning
  and asks for confirmation before resuming them all.
- **Specific jobs** (`pause <ids>`): only affects **queued** jobs — each is pulled
  out of the queue (state `paused`) so the scheduler skips it, while the rest of
  the queue keeps flowing. `resume <ids>` puts them back. A paused job still shows
  in `msc list` and can be `kill`ed or `remove`d like any other pending job.

### Limiting CPU jobs

By default CPU (non-GPU) jobs run with unlimited concurrency. Cap how many run at
once with `config cpu-limit`:

```bash
msc config cpu-limit 4   # at most 4 CPU jobs run concurrently; the rest queue
msc config cpu-limit     # show the current limit
msc config cpu-limit 0   # unlimited again (the default)
```

The cap is persisted and applies only to CPU jobs; GPU jobs remain bounded by the
GPU pool. Other daemon settings live under `msc config` too.

## Working directory & environment

When you queue a job, `msc` snapshots your current working directory and full
environment and runs the job under them. This means a job picks up whatever
shell environment was active at `add` time — for example after `pixi run nu`,
a virtualenv, or `conda activate` — so `python` resolves to the right
interpreter and project env vars are present.

## Environment variables

| Variable           | Effect                                                                    |
| ------------------ | ------------------------------------------------------------------------- |
| `MOCHI_HOME`       | Data directory for the queue state, logs, and socket. Defaults to the OS data dir. |
| `MOCHI_GPU_COUNT`  | Override detected GPU count (e.g. for testing without hardware).          |
| `MOCHI_GPU_VENDOR` | Override detected vendor: `nvidia` / `amd` / `none` (default `nvidia`).    |

## How it works

The client and daemon are the same binary. Running any command connects to a
per-user local socket; if no daemon is listening, the client spawns one as a
detached background process and retries. The daemon runs queued jobs, writing
each job's merged stdout/stderr to a log file, and persists the queue to
`state.json` (atomically, so a crash never corrupts it). If the daemon dies
while a job is running, that job is marked `failed` on the next start.

There is **one daemon per user** (the socket name is keyed on the username), so
all your shells share the same queue.

## Troubleshooting

### A progress bar (tqdm) or other output shows mojibake / garbled characters

A job's output is captured to a log file, not a terminal. Many programs encode
their output differently when it isn't a console: on a non-UTF-8 Windows (e.g. a
zh-TW system whose locale is `cp950`), Python writes a redirected stream using
that locale encoding, so non-ASCII characters — like tqdm's `█` bar glyph — are
mangled at the source. `msc` stores those bytes verbatim, so `cat`/`watch` then
shows the garbling. (Plain `python x.py > log 2>&1` has the same problem.)

Fix it by making the program emit UTF-8. For Python, set `PYTHONUTF8=1` (or
`PYTHONIOENCODING=utf-8`) in your shell before `msc add` — `msc` captures the
environment at `add` time, so the job inherits it:

```nu
with-env { PYTHONUTF8: "1" } { msc add -- python train.py }
# or persist it in your shell config (env.nu): $env.PYTHONUTF8 = "1"
```

For tqdm specifically, `tqdm(..., ascii=True)` draws the bar with ASCII and
sidesteps the encoding issue entirely; `tqdm(..., disable=None)` auto-hides the
bar when the output isn't a terminal.

### A rich progress bar doesn't show up in `msc watch`

[rich](https://github.com/Textualize/rich) detects that its output is not a
terminal — a job's output is captured to a log file — and by default **strips
control codes and disables animations**, so the progress bar never gets written
to the log. `msc watch` then has nothing to stream. (This is a different problem
from the tqdm garbling above, and has a different fix.)

Force rich to treat the captured output as an interactive terminal by
configuring the `Console` you hand to `Progress`:

```python
from rich.console import Console
from rich.progress import Progress

console = Console(
    force_terminal=True,      # emit ANSI control codes even though it isn't a tty
    force_interactive=True,   # enable the live animation (progress bar)
    legacy_windows=False,     # Windows: emit ANSI cursor moves instead of Win32 API
    width=120,                # optional: pin a width (see the wrapping note below)
)
with Progress(console=console) as progress:
    ...
```

- `force_terminal` + `force_interactive` are enough for a **single** progress
  bar (it redraws in place with a carriage return).
- `legacy_windows=False` is required on **Windows for multi-row** progress
  (several tasks, or a bar plus other live rows). Multi-row redraws rely on
  ANSI *cursor-up* escapes; without this, Windows rich falls back to the Win32
  console API — which does nothing when written to a file — so each refresh
  **stacks** onto new lines instead of updating in place.
- **Width:** when the output isn't a terminal rich can't detect its width and
  assumes 80 columns (or `COLUMNS`) to compute line wrapping and how far to move
  the cursor up. If the terminal you run `msc watch` in is *narrower* than that,
  the cursor math is off and the display garbles — pin a `width=` that your watch
  terminal comfortably fits, and use a VT-capable terminal (Windows Terminal is
  fine; the classic `conhost` is not).

View the result with `msc watch` (which replays the ANSI stream live so the bar
animates). `msc cat` dumps every captured frame at once, so a progress log looks
like a pile of overlapping redraws there — that's expected.

## License

MIT
