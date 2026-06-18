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
msc list                          # see all jobs and their state
msc info 3                        # full details for job 3
msc cat 3                         # print job 3's captured output (stdout + stderr)
msc watch 3                       # follow job 3's output live; Ctrl+C stops watching, not the job
msc kill 3                        # stop a running job, or drop a queued one
msc remove 3                      # remove a finished job from the list
msc clear                         # remove all finished/killed/failed jobs
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

## License

MIT
