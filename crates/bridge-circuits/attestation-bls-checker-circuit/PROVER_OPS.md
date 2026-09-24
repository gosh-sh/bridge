# Prover Ops — Quick Reference

Operational cheat sheet for running `primary_real_prover` (and similar long
halo2 runs) on a remote host over SSH without losing output or visibility.

---

## tmux — keep the prover alive across SSH disconnects

```bash
tmux new -d -s prover '<cmd>'              # start detached session named "prover"
tmux ls                                    # list sessions
tmux attach -t prover                      # attach (live view of stdout)
# Inside attached session:
#   Ctrl-b d        detach (process keeps running)
#   Ctrl-b [        enter scrollback mode → arrows / PgUp / PgDn / q to quit
#   Ctrl-b PgUp     quick scroll up
#   In copy mode: ? <text> Enter, then n / N to search

tmux capture-pane -t prover -p             # dump current screen
tmux capture-pane -t prover -pS -3000      # dump last 3000 lines of scrollback
tmux capture-pane -t prover -pS -          # dump full scrollback

tmux pipe-pane  -t prover -o 'cat >> ~/prover_pane.log'   # mirror live pane → file
tmux set-option -t prover remain-on-exit on               # keep pane after cmd exits

tmux send-keys     -t prover C-c           # send Ctrl-C to the running process
tmux kill-session  -t prover               # kill the session (and its process)
tmux kill-server                           # nuclear: kill all tmux on this host
```

## Robust launch recipe (immune to SSH drop, captures crashes)

```bash
cd ~/bridge/crates/bridge-circuits/attestation-bls-checker-circuit
RUN=run_$(date +%Y%m%d_%H%M%S); mkdir -p logs/$RUN

tmux new -d -s prover "exec > >(tee -a logs/$RUN/full.log) 2>&1; \
  echo '=== launch $(date -Is) ==='; \
  ./target/release/primary_real_prover 2000; \
  rc=\$?; echo \"=== exit code: \$rc at \$(date -Is) ===\"; exec bash"
tmux pipe-pane  -t prover -o "cat >> logs/$RUN/tmux_pane.log"
tmux set-option -t prover remain-on-exit on
```

`logs/$RUN/full.log` captures **stdout + stderr + panic backtrace + glibc
messages**. `logs/$RUN/tmux_pane.log` is an independent pane mirror. Either
survives if the other is lost.

## tail / head — watch the log

```bash
tail file.log                              # last 10 lines
tail -n 50 file.log                        # last 50 lines  (or: tail -50 file.log)
tail -n +100 file.log                      # from line 100 to end
tail -c 1K file.log                        # last 1 KB (bytes, not lines)
tail -f  file.log                          # follow, append-only
tail -F  file.log                          # follow, survives rotation/recreation
tail -f -n 100 file.log                    # show last 100 lines THEN follow
tail -f a.log b.log                        # follow multiple files
head -n 20 file.log                        # first 20 lines (mirror of tail)
```

## File timestamps & "is anything happening?"

```bash
ls -la file                                # mtime (default)
ls -la --full-time file                    # mtime with full precision
ls -lu file        ls -lc file             # atime / ctime
ls -lt dir/        ls -ltr dir/            # sort newest-first / oldest-first
ls -lt | head                              # 10 most recently modified

stat file                                  # all timestamps incl. Birth (creation)
stat -c '%y %n' file                       # mtime + name
stat -c '%Y'    file                       # mtime as Unix epoch

watch -n 5 'stat -c "%y %s bytes %n" params/primary_real_prover.log'   # live tick

find . -type f -mmin -30                   # modified in last 30 min
find . -type f -mtime -2                   # modified in last 2 days
find . -type f -newer reference_file        # files newer than reference
```

## Find / inspect the running prover

```bash
PID=$(pgrep -f primary_real_prover); echo "PID=$PID"
ps -o pid,user,pcpu,pmem,rss,etime,stat,cmd -p $PID    # CPU/mem/elapsed time
top -b -n1 -p $PID | tail -3                            # single CPU snapshot
top -p $PID                                              # interactive
pidstat -p $PID 1 5                                      # 5 × 1-sec deltas

readlink /proc/$PID/cwd                                 # process's working dir
lsof -p $PID | grep -E '\.log$|params/'                 # find the actual log file fd
sudo strace -p $PID -c -e trace=futex,read,write -- sleep 5  # 5-sec syscall summary
```

Healthy compute-bound prover: `%CPU ≈ cores × 100`, `STAT = Rl`, syscall
summary shows millions of `futex` calls in 5 seconds. State `D` (uninterruptible
I/O sleep) for a long stretch is a red flag.

## Diagnose silent death

```bash
pgrep -af primary_real_prover                                                      # alive?
sudo dmesg -T | grep -iE 'killed process|oom|primary_real|out of memory' | tail    # OOM-killer
sudo journalctl -k --since '6 hours ago' | grep -iE 'oom|primary_real' | tail      # kernel log
ls -la core* /var/lib/systemd/coredump/ 2>/dev/null                                # core dumps
coredumpctl list 2>/dev/null | tail                                                # systemd coredumps
free -h                                                                             # current memory pressure
df -h .                                                                             # disk pressure
```

`dmesg` showing `Killed process … (primary_real_prover) … total-vm:` with a
`total-vm:` line is the OOM-killer. No userspace logging catches SIGKILL —
the kernel ring buffer is the only trace.

## Stop the prover

```bash
tmux send-keys -t prover C-c                 # graceful SIGINT
tmux kill-session -t prover                  # kill session + its process
pkill -INT  -f primary_real_prover; sleep 5  # polite
pkill -TERM -f primary_real_prover; sleep 5  # firmer
pkill -KILL -f primary_real_prover           # last resort
pgrep -af primary_real_prover                # verify it's gone
```

## Useful env vars when launching

```bash
ARTIFACT_DIR=/scratch/primary_bench         # where VK/PK/proof/log go
PARAMS_DIR=/scratch/srs                     # where halo2 caches SRS
LOG_FILE=/abs/path/run.log                  # pin log file location (absolute!)
RAYON_NUM_THREADS=16                        # cap rayon parallelism
JEMALLOC_SYS_WITH_MALLOC_CONF=narenas:1     # reduce jemalloc memory overhead
```

`ARTIFACT_DIR` and `LOG_FILE` default to **relative paths from the CWD where
you launched the binary** — not relative to the binary location. Use absolute
paths to remove ambiguity.

## Alternative to tmux: systemd-run (best for unattended long runs)

```bash
systemd-run --user --unit=primary_prover --description='primary real prover' \
  --setenv=RAYON_NUM_THREADS=16 \
  ./target/release/primary_real_prover 2000
journalctl --user -u primary_prover -f      # live tail, survives disconnects
systemctl --user status primary_prover       # exit code, RSS peak, OOM info
systemctl --user stop primary_prover         # stop
```

journald is durable, immune to SSH disconnects, and `systemctl status` tells
you exactly why the unit exited (signal, exit code, OOM). Prefer this over
tmux for runs measured in hours/days.

## What the binary itself logs vs. doesn't

| Thing happening on the host                          | In `primary_real_prover.log`? |
|------------------------------------------------------|-------------------------------|
| Every `logln!()` line (timings, step headers)        | Yes — flushed per line        |
| A Rust `panic!` / `unwrap()` / `assert!`             | Yes — via the panic hook      |
| `eprintln!`, library log output, halo2 progress      | **No** — stderr only          |
| SIGKILL from the OOM-killer                          | **No** — process dies first   |
| Segfault / abort in C code (jemalloc OOM, FFI)       | **No** — bypasses panic hook  |
| Anything before `init_log()` returns                 | **No** — log not open yet     |
| `tmux new -d -s prover '<cmd>'` after `<cmd>` exits  | Gone — pane closes, scrollback discarded unless `remain-on-exit on` |

This is why the launch recipe above ALSO redirects to `logs/$RUN/full.log` at
the shell level — it captures the rows the in-process logger can't.

## Why long silences between log lines are normal

Between consecutive `logln!` calls in `primary_real_prover.rs` there are
compute-bound phases with **no** intermediate output (halo2 doesn't emit
progress):

```
Step 3: prove + verify
[timing] circuit construction: ...                  ← last line you see
let proof = Proof::create_for_circuit_from_paths::<...>(...);  ← entire prove runs here
[timing] proof generation: ...                      ← next line, hours later for k=2000
```

For `max_signers=2000`: `keygen_pk` and proof generation are each easily
multi-hour. A static log with `%CPU ≈ cores × 100` is the prover working as
designed, not hanging.
