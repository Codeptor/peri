# kestrel deployment (systemd user units)

Two units live here, plus the log-hygiene config for the nohup era they replace:

| File | What it supervises | State |
|---|---|---|
| `kestreld.service` | the paper-trader daemon | supervising since R2 — **pending rename cutover**: the installed unit is still `traderd.service` running the old binary + `traderd.db`; lead swaps it (see below) |
| `tg-news-pipe.service` | `tg_news_pipe.py` → `POST /ingest/news` | **not started** — pipe still runs under `nohup`, lead cuts over |
| `logrotate-botta.conf` | `smoke.log`, `tg_news_pipe.log` | config only — `logrotate` is not installed yet |

`kestreld.service` is the supervised way to run the daemon. It replaces the
`setsid nohup … &` launch, which survives nothing: a panic, an OOM kill or a
reboot leaves the trader dead and silent until someone notices.

PAPER-ONLY still holds — the units run the same binary and the same script with
the same `kestreld.toml`; nothing here touches wallets or live orders.

## Rename cutover: `traderd` → `kestreld` (lead runs this once)

The repo has been renamed but the running system has not. Until these steps run,
`traderd.service` is still supervising the old binary against `kestreld/traderd.db`.
Build first so `ExecStart` resolves, and move the DB only while the daemon is stopped —
that is what preserves the ledger.

```bash
cd ~/botta/kestreld && cargo build --release          # produces target/release/kestreld
systemctl --user stop traderd                          # ledger is now quiescent
mv traderd.db kestreld.db                              # data preserved, new name
for s in wal shm; do [ -e "traderd.db-$s" ] && mv "traderd.db-$s" "kestreld.db-$s"; done; true
cp deploy/kestreld.service ~/.config/systemd/user/kestreld.service
systemctl --user disable --now traderd
rm ~/.config/systemd/user/traderd.service
systemctl --user daemon-reload
systemctl --user enable --now kestreld
curl -s 127.0.0.1:7411/api/health | jq                 # ok:true, markets_tracked > 0
journalctl --user -u kestreld -n 30                    # no panics, store opened
```

A clean `SIGINT` stop checkpoints and removes the `-wal`/`-shm` sidecars, so the loop
above is a no-op in the normal case; it exists for a hard kill. The old
`target/release/traderd` binary can be deleted after the new unit is verified green.

## kestreld — install

```bash
mkdir -p ~/.config/systemd/user
cp ~/botta/kestreld/deploy/kestreld.service ~/.config/systemd/user/kestreld.service
systemctl --user daemon-reload
```

The unit uses `%h` (the user's home), so it needs no editing as long as the repo
lives at `~/botta`. Build the release binary first — `ExecStart` points at it:

```bash
cd ~/botta/kestreld && cargo build --release
```

## kestreld — enable + start

```bash
systemctl --user enable --now kestreld    # start now + on every login
loginctl enable-linger "$USER"            # keep it running with no session open
```

**WSL2 note:** `loginctl enable-linger` is what makes the daemon outlive a
closed terminal, and it only works when systemd is actually PID 1 in the
distro. Check `/etc/wsl.conf` contains:

```ini
[boot]
systemd=true
```

and restart the distro (`wsl.exe --shutdown` from Windows) if you had to add it.
Verify with `systemctl --user is-system-running` — anything other than
`running` / `degraded` means user units are not available and the fallback
below applies. WSL2 has no login-at-boot, so the daemon starts when the distro
is first entered, not when Windows boots.

## kestreld — operate

```bash
systemctl --user status kestreld          # state, pid, uptime, last log lines
systemctl --user restart kestreld         # after a rebuild
systemctl --user stop kestreld
systemctl --user disable --now kestreld   # stop + never auto-start again
journalctl --user -u kestreld -f          # live log
journalctl --user -u kestreld --since -1h # last hour
journalctl --user -u kestreld -p warning  # warnings and errors only
```

Health check, unchanged:

```bash
curl -s 127.0.0.1:7411/api/health | jq
```

## What the kestreld unit gives you

- `Restart=on-failure` + `RestartSec=5` — a panic (release profile is
  `panic=abort`, so a panic is a non-zero exit) or a crash comes back in 5s.
- `KillSignal=SIGINT` — `main()` shuts down on the ctrl-c handler; SIGTERM
  would bypass it.
- Journal logging — no more `smoke.log` growing unbounded; `journalctl`
  handles rotation and filtering.
- `Environment=RUST_LOG=info` — override per-run with
  `systemctl --user edit kestreld` if a module needs `debug`.

Secrets keep coming from `~/botta/.env` (0600) via dotenvy, which reads `./.env`
and `../.env` relative to `WorkingDirectory`. Do not add an `EnvironmentFile=`
line: dotenvy never overrides variables already in the process environment, so
a stale value injected by systemd would silently win over `.env`.

## kestreld fallback (no systemd available)

If user units are unavailable (systemd not enabled in WSL2), the old launch
still works and stays supported as a fallback only:

```bash
cd ~/botta/kestreld
setsid nohup ./target/release/kestreld --config kestreld.toml >> smoke.log 2>&1 &
kill -INT "$(pgrep -x kestreld)"   # stop
```

It has no restart supervision — the R2 watchdog will still alert on Telegram
when the daemon wedges, but nobody will restart it for you.

---

## tg_news_pipe — install (NOT yet cut over)

`tg-news-pipe.service` supervises the Telethon sidecar that forwards news
channels and the `trenchers_den` signal group into `POST /ingest/news`. It runs
the pipe exactly as the ops table documents:

```
uv run --with telethon --with httpx python tg_news_pipe.py
```

with `WorkingDirectory=%h/botta` — the repo root, not `kestreld/`, because the
script opens `./.env` and `./kestreld/kestreld.toml` and writes
`./botta_news.session` relative to cwd.

**The pipe is currently running under `nohup`. Do not `start`/`enable` this unit
until the lead has stopped that process — two pipes on one Telegram session
means duplicate `/ingest/news` posts and two writers on
`botta_news.session`.** Install (copy + `daemon-reload`) is safe on its own; it
starts nothing.

```bash
mkdir -p ~/.config/systemd/user
cp ~/botta/kestreld/deploy/tg-news-pipe.service ~/.config/systemd/user/tg-news-pipe.service
systemctl --user daemon-reload
systemd-analyze --user verify ~/botta/kestreld/deploy/tg-news-pipe.service   # silent = clean
```

### Cutover (lead runs this)

```bash
pgrep -af '[t]g_news_pipe.py'                  # confirm the nohup pipe + its uv parent
pkill -f '[t]g_news_pipe.py'                   # bracket trick — never let pkill -f match its own cmdline
pgrep -af '[t]g_news_pipe.py' || echo "clear"  # must print `clear` before continuing
systemctl --user enable --now tg-news-pipe
journalctl --user -u tg-news-pipe -f           # expect: `tg_news_pipe running · N channels + 1 signal groups`
```

`pkill -f` matches the `uv run` parent and the `python` child; verify both are
gone (`uv` re-execs the child, so killing only the child is not enough).

Same `loginctl enable-linger "$USER"` + `systemd=true` in `/etc/wsl.conf`
prerequisites as kestreld — see the WSL2 note above.

### tg_news_pipe — operate

```bash
systemctl --user status tg-news-pipe
systemctl --user restart tg-news-pipe
journalctl --user -u tg-news-pipe -f
journalctl --user -u tg-news-pipe | grep -F '[warn] drop news'   # kestreld was down / rejecting
systemctl --user disable --now tg-news-pipe                      # back to manual
```

### What the pipe unit gives you

- `Restart=on-failure` + `RestartSec=10` — cures the documented daemonization
  gotcha where `setsid -f nohup` lets the pipe die silently within minutes.
  Supervision is the point: it comes back instead of going quiet.
- `Environment=PYTHONUNBUFFERED=1` — the script reports with `print()`, and
  stdout to journald is a pipe, so without this its output is block-buffered and
  lines arrive late or are lost when the process dies.
- `KillSignal=SIGINT` — Python installs no default SIGTERM handler, so SIGTERM
  kills the interpreter mid-write; SIGINT raises `KeyboardInterrupt`, unwinds
  `asyncio.run()` and lets Telethon close its SQLite session cleanly.
- `StartLimitBurst=5` / `StartLimitIntervalSec=300` — a missing or expired
  `botta_news.session` fails instantly under systemd (stdin is `/dev/null`, so
  Telethon's interactive login cannot prompt). The unit gives up into `failed`
  where `status` shows it, instead of hammering Telegram auth forever.

**First login must happen by hand.** If `botta_news.session` is ever lost, run
the pipe once in a real terminal (`cd ~/botta && uv run --with telethon --with
httpx python tg_news_pipe.py`) so the login code prompt works, then start the
unit.

No `EnvironmentFile=` here either: `load_env()` uses `os.environ.setdefault`, so
anything systemd injects would silently win over `.env` — the same invariant
that keeps dotenvy out of kestreld's unit.

---

## Log hygiene

### smoke.log / tg_news_pipe.log (nohup era)

`kestreld/smoke.log` is append-forever from the `setsid nohup … >> smoke.log`
launch — nothing truncates it. It is **52 KB** as of this writing and its last
write was 20:29 IST, before the daemon went under systemd at 21:32, so it is
already frozen: **under systemd both daemons log to journald, so these files
stop growing after cutover anyway.** `logrotate-botta.conf` exists to age out
what is on disk and to bound the fallback path, which still appends to
`smoke.log` when systemd is unavailable.

`logrotate-botta.conf` covers `kestreld/smoke.log` and `tg_news_pipe.log`:
weekly, `rotate 4`, `compress`, `copytruncate`. `copytruncate` is the load-bearing
one — a running nohup process holds the fd open and appends by offset, so
renaming the file would leave it writing to an unlinked inode; copy-then-truncate
keeps the inode and the writer keeps logging.

**`logrotate` is not installed on this box** (`pacman -Q logrotate` → not found):

```bash
sudo pacman -S logrotate
```

Run it as the user, with a user-writable state file — the default
`/var/lib/logrotate/logrotate.status` is root-only:

```bash
logrotate -s ~/.local/state/logrotate-botta.status ~/botta/kestreld/deploy/logrotate-botta.conf
logrotate -d -s ~/.local/state/logrotate-botta.status ~/botta/kestreld/deploy/logrotate-botta.conf  # dry run
```

**Wiring — systemd user timer (preferred here).** `cronie` is not installed
either, and the user manager is already running:

```bash
cat > ~/.config/systemd/user/logrotate-botta.service <<'EOF'
[Unit]
Description=logrotate botta nohup logs

[Service]
Type=oneshot
ExecStart=/usr/bin/logrotate -s %h/.local/state/logrotate-botta.status %h/botta/kestreld/deploy/logrotate-botta.conf
EOF

cat > ~/.config/systemd/user/logrotate-botta.timer <<'EOF'
[Unit]
Description=weekly logrotate for botta nohup logs

[Timer]
OnCalendar=weekly
Persistent=true

[Install]
WantedBy=timers.target
EOF

systemctl --user daemon-reload
systemctl --user enable --now logrotate-botta.timer
systemctl --user list-timers logrotate-botta.timer
```

`Persistent=true` runs a missed rotation on next login — WSL2 is off more than
it is on.

**Wiring — crontab (alternative).** Needs `sudo pacman -S cronie` plus
`systemctl enable --now cronie`, then `crontab -e`:

```cron
0 4 * * 0 /usr/bin/logrotate -s $HOME/.local/state/logrotate-botta.status $HOME/botta/kestreld/deploy/logrotate-botta.conf
```

### journald caps (documented only — do not edit system files)

Journals are **854.7 MB** right now and `/etc/systemd/journald.conf` is stock
(every key commented), so the default `SystemMaxUse` applies: 10% of the
filesystem, capped at 4 GB. Now that both daemons log to the journal, that is
the growth path worth bounding.

The system-wide fix needs root and is **not applied here** — it is documented so
the lead can decide:

```ini
# /etc/systemd/journald.conf
[Journal]
SystemMaxUse=200M
```

then `sudo systemctl restart systemd-journald`.

**User-level equivalent:** there is no per-user journald size setting — user
journals live in the system journal and obey `SystemMaxUse`. What a user *can*
do without root is vacuum on demand:

```bash
journalctl --user --disk-usage
journalctl --user --vacuum-size=200M   # deletes archived user journals
journalctl --user --vacuum-time=14d
```

A vacuum deletes logs, so run it deliberately. It can be wired to a weekly user
timer the same way as the logrotate timer above if root access never happens.
