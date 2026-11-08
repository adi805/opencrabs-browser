---
document_type: evidence
project: "OpenCrabs Browser: web-opening engine"
source_run: "https://github.com/adi805/opencrabs-browser/actions/runs/42445976171"
commit: 2633095
measured: "2026-11-08 11:18-11:24 WIB"
---

# Evidence: native CDP surface served by the shipped binary

AC-013 and AC-014 were, until this file, backed only by in-process tests. This
record closes them with the **binary built in GitHub Actions**, not with a local
`cargo build`, because the laptop is not where release artifacts come from.

## What was built and where it came from

| Step | Receipt |
|---|---|
| Run | `42445976171`, head `2633095`, conclusion `success` |
| Artifact | `browserd-0.1.0-x86_64-unknown-linux-gnu.tar.gz` |
| Checksum | `sha256sum -c` prints `OK` |
| Binary | `browserd 0.1.0`, ELF 64-bit PIE, stripped, **23,769,360 bytes** |
| Binary sha256 | `474d24432991b90c48a4f4b629a0385a4f80283829ba0d1f89abe0f10b9671ee` |

The run that produced this binary needed two fixes first, and both were found by
verification rather than by reading:

1. `e618729` fixes a real boot failure. The daemon refused to start with
   `[policy] tidak sah untuk mesin yang terpasang (rust-native): - nama mesin
   'rust-native' tidak dikenal`, because `EngineKind` in `crates/policy/src/select.rs`
   knew only `shell`, `full`, and `oxi`. The log line is quoted from
   `/tmp/oc-native-ac13/daemon.log` before the fix.
2. `2633095` fixes the CI packaging step. Run `42445740212` built the binary
   fine and then died in `Paketkan` with a bare `exit 1`: cargo names its output
   directory after the built-in profile it maps to, so `--profile dev` lands in
   `target/debug/` while the step looked for `target/dev/`, and `set -eu` killed
   the step before it could print anything.

## AC-013: `/json/version` names RustNativeEngine

Instance: `/tmp/oc-native-ac13/browserd.toml`, `bind = 127.0.0.1:9234`,
`[engines.native] kind = "rust-native"`, `max_tabs = 99`. Nothing on 9222, 9224,
or 9225 was touched.

```json
{"Browser":"RustNativeEngine/0.2.0","Protocol-Version":"1.3",
 "User-Agent":"Mozilla/5.0 (X11; Linux x86_64) RustNativeEngine/0.2.0",
 "V8-Version":"","WebKit-Version":"",
 "webSocketDebuggerUrl":"ws://127.0.0.1:9234/devtools/browser/ec8d618e5f6dfa337785d"}
```

A full CDP round trip through that socket, driven by `/tmp/ac13-probe.mjs`, exits
**0** with these lines:

```
ok    /json/version Browser=RustNativeEngine/0.2.0 Protocol=1.3
ok    /json/protocol domains=6 commands=24
ok    /json/new id=3
ok    Page.getFrameTree frameId=FRAME-3
ok    Page.setDocumentContent receipt.delta={"dom":true,"judul":true,"node":8,"url":false}
ok    DOM.getDocument depth=-1 nodes=12
ok    Runtime.evaluate 6*7 -> {"description":"42","type":"string","value":"42"}
ok    Runtime.evaluate document.title -> {"description":"AC13","type":"string","value":"AC13"}
ok    /json/close -> Target is closing
ok    /json/list setelah tutup = []
SEMUA LOLOS nodes=12 commands=24
```

Two things the probe taught, both worth recording because the first two runs
failed for reasons that were the probe's fault, not the daemon's:

- `DOM.getDocument` defaults to `depth = 1`, which is what Chrome does. A bare
  `{}` returns the root plus its direct children, so 2 nodes, not 12. The probe
  now passes `depth: -1`.
- `Runtime.evaluate` **does** return a value: `result.result.value` is `"42"`.
  The first print was truncated at 200 characters and cut the `result` key off,
  which made it look like the daemon only returned a receipt. Reading the
  handler in `crates/cdp/src/native_wire/state.rs:585` settled it.

`/json/protocol` advertises 6 domains and 24 commands, and deliberately does not
copy Chrome's full schema: advertising `Network.*` or `Page.captureScreenshot`
would make clients call methods that are certain to fail.

## AC-014: nothing survives a SIGTERM

| Measurement | Before SIGTERM | After SIGTERM |
|---|---|---|
| Processes spawned by the daemon (`ps --ppid`) | 0 | 0 |
| Daemon pid alive | yes | **gone** |
| Listeners on `127.0.0.1:9234` | 1 | **0** |
| Processes carrying this config path | 1 | **0** |
| `chrome` process count on the host | 53 | **53 (delta 0)** |
| Last log line | `sesi CDP selesai to_engine=6 from_engine=5` | `semua mesin dimatikan` |

The native engine runs in-process: the config logs `pid = None` and an
ephemeral internal port (`http = http://127.0.0.1:39499`), so there is no browser
child to leak in the first place. That is the AC's point, and it is why the
"zero Chrome processes" claim is stronger here than for `chrome-full`.

### The one honest caveat

`~/.opencrabs/projects/oc-browser-leak-check.sh` exits **1** on this host, and it
exited 1 **before** the SIGTERM as well. Its single hit is not from this run:

| Field | Value |
|---|---|
| pid | `598510` |
| cmdline | `chrome-headless-shell --remote-debugging-port=38247 --user-data-dir=/tmp/ocb-biaya-shell-598504` |
| started | `Sat Nov 7 21:46:07 2026` (13h 37m old at measurement) |
| parent | `1292` = `/usr/lib/systemd/systemd --user`, i.e. reparented, so its launcher is dead |
| RSS | 14 MB |

It is a genuine orphan from an earlier test run under a different name
(`biaya-shell`), it predates this instance by 13 hours, and the host-wide
`chrome` count is identical before and after the SIGTERM. The script is global by
design, so its exit code cannot separate this instance's contribution from a
pre-existing orphan. It is left alive on purpose: killing another session's
process without asking is not cleanup.

## Memory: the native engine is cheap because it does less

Measured on the same instance and the same GitHub-built binary. The native
engine runs in-process, so the daemon's own RSS **is** the engine's footprint.

| Moment | RSS | Delta |
|---|---|---|
| 0 tabs, fresh process | 20 MB | baseline |
| 25 tabs, `about:blank` | 27 MB | +7 MB |
| 99 tabs, `about:blank` | 30 MB | +10 MB over baseline (0.10 MB/tab) |
| 99 tabs after close, allocator holding pages | 29.9 MB | reset point for the next row |
| 25 tabs, 403-node document each | 32.9 MB | +3.0 MB (0.12 MB/tab) |
| 99 tabs, 403-node document each | 37.6 MB | +7.7 MB over the reset point (0.08 MB/tab marginal) |
| 100th tab | refused | `mesin 'native' sudah memakai 99 dari batas 99 tab (max_tabs = 99)` |

The documents were `data:text/html` pages of one `h1` plus 200 `<p>baris-N</p>`,
so roughly 403 nodes per tab, loaded through the same `Page` path a client uses.
99 of them cost **37.6 MB in total**, against the measured **3.10 MB per tab**
for `chrome-full` (310 MB for the same 99).

**This is not a like-for-like win, and saying so matters more than the number.**
The native engine has no rasteriser, no stylesheet cascade, no text measurement
and no DOM events (`engine::native::CELAH`), so a tab here is a tree and a set of
boxes, not a rendered page. Chrome's 3.10 MB buys a compositor, a GPU process, a
network stack and a V8 isolate per tab. The honest claim is: the pool is cheap
by construction, and the acceptance criterion that it stays under 100 MB is
reachable **if** the missing capabilities are added without a per-tab process or
an unbounded cache. It is not yet evidence that a fully featured native engine
fits in 100 MB.

## What this does not prove

The surface answers CDP. The engine behind it is still missing ten capabilities,
and they are listed in code at `engine::native::CELAH`
(`crates/engine/src/native/mod.rs:38`): CSS selectors, stylesheet cascade,
property inheritance, text measurement, DOM events, rasterisation, HTTP fetch,
typed `Runtime.evaluate` returns, a real DOM inside the isolate, and CDP events.
`Page.navigate` therefore only serves `about:blank` and `data:text/html`, and no
screenshot can be produced. AC-013 is about the surface; it is not evidence that
this engine can open a real page.