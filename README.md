# keepalive

Babysits one process: auto-restarts it on crash with exponential backoff,
capturing its stdout/stderr to a log file — `supervisord` for people who
don't want Python, a config file, or a whole process group manager just
to keep one worker script alive.

## Usage

```bash
keepalive "python3 worker.py --queue default" --log worker.log
keepalive "node server.js" --log app.log --restart always --max-restarts 10
keepalive "./flaky-job.sh" --log job.log --backoff-base 500ms --backoff-max 30s
```

`--restart on-failure` (the default) only restarts on a nonzero exit or a
signal; `--restart always` restarts even after a clean exit 0 — the same
distinction `supervisord`'s `autorestart` setting makes. `--max-restarts`
is unset (unlimited) by default. Forwards `SIGTERM`/`SIGINT` it receives
to the currently-running child rather than just killing itself and
orphaning it.

## Backoff

Exponential, starting at `--backoff-base`, multiplied by
`--backoff-multiplier` (default `2.0`) after each consecutive failure, up
to `--backoff-max`. A run that stays up at least `--reset-after` resets
the delay back to `--backoff-base` on its *next* crash, so a process
that's been healthy for hours doesn't inherit a huge delay from a crash
loop it had days ago.

## Status: built and verified, including two real bugs caught by actually running the tests

- **35 unit tests** (`cargo test --lib`) across `restart` (the pure
  decision logic — `Stop`/`GiveUp`/`Restart(delay)` for every
  policy/max-restarts/backoff combination, with no process spawning or
  sleeping involved, so every branch is directly testable), `backoff`
  (the exponential math and its cap), `duration` (parsing `500ms`/`1s`/
  `2m` argument strings), and `supervisor` (the real OS-touching layer —
  see below).
- **Two real bugs found and fixed by actually running these tests, not
  just writing them** (the crate compiled fine — these were logic bugs
  the test suite itself caught):
  1. The "giving up" log message printed `state.total_restarts()`, which
     is already one *past* the configured limit at give-up time (it's
     incremented for the failure that triggered giving up, which itself
     never actually got restarted) — so `--max-restarts 3` produced
     "giving up after 4 restart(s)" instead of 3. Fixed to report
     `config.max_restarts` (the limit that was actually reached)
     instead of the internal counter.
  2. A test asserting on the number of real process spawns filtered log
     lines with `.contains("starting")` — which also matches every
     `"restarting in ...s"` backoff banner, since "restarting" literally
     contains "starting" as a substring. This silently double-counted
     every backoff decision as a second spawn (7 matches instead of the
     real 4) until the test was actually run. Fixed to filter on
     `"starting '"` (the trailing quote from the real spawn banner's
     format string), which the backoff banner never contains.
- **`supervisor`'s tests spawn real child processes, not mocks** (13 of
  the 35): a real process that exits nonzero is genuinely restarted the
  configured number of times (verified via a real counter file each
  spawn appends a byte to, not an in-memory mock); a clean exit-0 process
  is *not* restarted under the default policy but *is* under `--restart
  always`; real stdout and stderr both genuinely land in the log file;
  real backoff delays between real spawns are measured with real
  wall-clock timestamps and confirmed to actually grow; a `SIGTERM`-style
  shutdown flag set mid-run stops supervision without another restart;
  and `current_pid` reflects the real live child's actual PID throughout.
- **Live-verified against the actual compiled binary end to end**: ran
  `keepalive` against a real always-failing command with
  `--max-restarts 2`, and the real log file showed exactly 3 real
  spawns (1 initial + 2 restarts), real growing backoff delays (50ms
  then 100ms, matching the configured base and multiplier), and the
  now-correct `"giving up after 2 restart(s)"` message — the exact fix
  from bug #1 above, confirmed against a real run, not just the unit
  test.

**Not done / deliberately deferred**: process groups (a child that
forks its own children isn't tracked — `SIGTERM` only reaches the direct
child `sh -c` spawns, not further descendants); log rotation (this
workspace's separate `rotatelog` tool is the intended pairing — pipe or
schedule it against `--log`'s output rather than this tool reimplementing
rotation); and a config file — every option is a CLI flag, on purpose,
matching the "no config file" framing in this tool's own pitch.
