---
document_type: measured-audit
engine: google-chrome 156.0.8078.79, headed, inside Xvfb
measured_in: development laptop, Linux Mint 22.3, 4 cores, 7.6 GB RAM, load 17-20 during the runs
measured_at: "2026-11-07 13:29 WIB"
supersedes: nothing, this adds stability and cost measurements to BUKTI-FULL 071126-0734.md
---

# chrome-full: identity, stability and cost audit

This file answers three questions with measured numbers only: is the `chrome-full`
engine really full Chromium, does it stay reliable across repeated boots, and what
does one instance cost. Every number below was read from a tool result during the
audit; nothing is inferred from Chromium documentation.

## 1. Identity: full Chrome, headed, on a virtual display

The claim "power full" is only true if the process is the real browser. Three
independent checks say it is.

| Check | Command | Result |
|---|---|---|
| Resolved binary | `readlink -f /usr/bin/google-chrome` | `/opt/google/chrome/google-chrome` |
| Real browser payload | `ls -l /opt/google/chrome/chrome` | 302792008 bytes (302 MB) |
| Headless shell on disk | `ls /opt/google/chrome/chrome-headless-shell` | not present in that directory |
| User agent in the page | probe | `... (X11; Linux x86_64) ... Chrome/156.0.0.0 Safari/537.36` |
| `HeadlessChrome` in UA | `grep -c HeadlessChrome` on 5 logs | 0 in all five |
| `headless` anywhere in logs | `grep -ci headless` on 5 logs | 0 in all five |

The user agent is the interesting one: `chrome-headless-shell` reports
`HeadlessChrome`, and the probe reports plain `Chrome/156` with an `X11` platform
token. That token cannot be produced by the headless shell.

Live process scan while the engine was up (script read `/proc/<pid>/cmdline` and
`/proc/<pid>/environ` directly, pattern built at runtime so the scanner cannot
match itself):

```
exe: /opt/google/chrome/chrome
flag --headless: []
flag --user-data-dir: ['--user-data-dir=/tmp/ocb-penuh-probe-285647']
flag --window-size: ['--window-size=1920,1080']
DISPLAY: ['DISPLAY=:102']
jumlah argumen: 13
```

Thirteen arguments, no `--headless`, no automation-hiding flag, `DISPLAY` pointing
at the Xvfb that the engine started for itself.

## 2. Stability across five consecutive boots

The first measurement of this adapter was two runs, one of which timed out at 30 s
because 26 leaked browsers from earlier test runs had eaten the RAM. That is fixed
(two commits: `impl Drop for Proc` and SIGTERM teardown in the test helpers), so
the adapter now gets tested the way it will be used: repeatedly, on a busy host.

| Run | Boot | RSS at idle-with-page | Processes | Whole test |
|---|---|---|---|---|
| 1 | 2257 ms | 1680 MB | 17 | 6.94 s |
| 2 | 2432 ms | 1501 MB | 15 | 7.07 s |
| 3 | 2882 ms | 1494 MB | 15 | 11.60 s |
| 4 | 2696 ms | 1500 MB | 15 | 6.31 s |
| 5 | 2706 ms | 1507 MB | 15 | 7.34 s |

- 5 of 5 passed, 0 failed.
- Boot: min 2257 ms, max 2882 ms, mean 2595 ms, spread 625 ms.
- RSS: 1494 to 1680 MB, mean 1536 MB.
- Host load during the runs: 17.6 to 20.2 on 4 cores, driven by unrelated work
  (a Remotion render, a `dotnet` test run, `ffmpeg`). The adapter booted in under
  3 s anyway.
- Every run used display `:102`, which means each boot allocated and then released
  the same display number instead of leaking `:N` sockets.

## 3. Capability, re-measured

Same four capabilities as `BUKTI-FULL 071126-0734.md`, unchanged in five runs:

| Probe | Result |
|---|---|
| `webdriver` | `false` |
| `layout` | title read, element box `{x:0, y:21.44, width:1200, height:600}`, screenshot 12820 base64 chars |
| `script` | page inline script ran: `dataset.skrip` = `ya` |
| `cookies` | `probe=ya` written via `Network.setCookie`, read back identical |
| `input` | `Input.dispatchMouseEvent` landed: `dataset.klik` = `ya` |

`webdriver=false` is one signal, not a proof of undetectability. Nothing in this
repo claims the engine is undetectable, and no anti-detection flag is passed.

## 4. Leak check

`~/.opencrabs/projects/oc-browser-leak-check.sh` counts chrome processes carrying
`--user-data-dir=/tmp/ocb-*`, the marker only test engines use.

| Moment | Result |
|---|---|
| Before the teardown fix | 26 engines alive, ~4.9 GB |
| After each of the 5 probe runs | `BERSIH: nol mesin test tersisa`, exit 0 |
| After the full workspace gate | `BERSIH: nol mesin test tersisa`, exit 0 |

## 5. Full gate

`cargo xtask check`, 94 s wall clock:

```
-> fmt     ok
-> clippy  ok
-> test    ok
```

Zero `gagal` lines. After the gate, production ports were re-probed:

| Port | Before | After |
|---|---|---|
| 9222 (production chrome-headless-shell) | 200 | 200 |
| 9224 (browserd API) | 200 | 200 |
| 9226 (nested interactive browser) | 200 | 200 |

## 6. What one instance actually costs

The leak is gone; the weight is not. These are different claims and only the first
one is fixed.

| Path | Measured footprint | Note |
|---|---|---|
| Test engines after a run | 0 | was 26 |
| Idle interactive browser, 9226 tree | 276 MB | real Chrome, one page open |
| Production headless shell, 9222 tree | 214 MB | idle, no page |
| `chrome-full` engine, booted by the probe | 1494-1680 MB, 15-17 procs | one page, Xvfb included |

So "light" is true for the system and false for the engine. Running `chrome-full`
costs roughly ten times the headless shell. That is the price of a real window,
real input dispatch and a real layout engine, and it is the reason `shell` stays
the default path.

## 7. Not measured, therefore not claimed

| Question | Why it is open |
|---|---|
| WebGL / GPU acceleration | Xvfb has no GPU; the adapter passes no GPU flags. Renderer strings were not sampled, so nothing is claimed about hardware acceleration. |
| Behaviour after hours of idle | The longest observation here is 11.6 s of test. Memory creep over an hour is unmeasured. |
| Several concurrent full engines | Only one engine ran at a time. Two engines on 4 cores / 7.6 GB is untested. |
| Anti-bot detection beyond `navigator.webdriver` | One signal was measured; detection is a moving target and no claim is made. |
| Automatic census inside `cargo xtask check` | The leak check is still a manual script. A test killed from outside never runs its destructor, so the gate can still pass with orphans alive. |
