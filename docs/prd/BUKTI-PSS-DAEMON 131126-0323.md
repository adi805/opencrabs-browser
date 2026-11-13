# PSS daemon 99 tab dengan cold store SQLite

Measured 2026-11-13 03:0x-03:2x WIB on the laptop (Linux, `browserd` debug build
from `/mnt/hdd/OpenCrabs/build/cargo/debug`). This file exists because the
number it records **contradicts** the budget claim in the PRD, and a number that
is not written down gets quoted from memory three sessions later.

## Setup

| Item | Value |
|---|---|
| Binary | `browserd` from `/mnt/hdd/OpenCrabs/build/cargo/debug` (dev profile, built from `35c215e` + the `tabstore` fix in this commit) |
| Endpoint | `127.0.0.1:9236` (data), `127.0.0.1:9237` (control) |
| Config | `/tmp/oc-pss99/browserd.toml`, `[engines.native] kind = "rust-native"`, `max_tabs = 99`, `idle_reap_secs = 0`, own `store_db` / `store_key` / `control_token` under `/tmp/oc-pss99/` |
| Store | keygen'd fresh per run (`browserd keygen`), deleted between runs |
| Workload | 99 tabs, each `Page.setDocumentContent` with 1500 `div` of inline text = 3005 nodes, same document the budget and rehydration tests use |
| Harness | `/tmp/pss-99.mjs` reads `/proc/<daemon-pid>/smaps_rollup` `Pss:` directly, and `/json/list` after the last tab |
| Untouched | production 9222, live daemon 9224/9225, `~/.opencrabs/*` |

## Results

| Run | PSS baseline | PSS at 99 tabs | Marginal | RSS at 99 tabs | Targets in `/json/list` | Wall clock |
|---|---|---|---|---|---|---|
| 1 | 21.54 MB | **105.07 MB** | +83.53 MB | 107.35 MB | 99 | 50.26 s |
| 2 | 21.42 MB | **103.17 MB** | +81.75 MB | 105.46 MB | 99 | 54.20 s |
| In-process test (`native_99_tabs_budget_sqlite`) | 8.0 MB | **59.5 MB** | +51.5 MB | n/a | n/a (pool only) | 39.34 s |

Both daemon runs **exceed the 100 MB ceiling** (NFR-001 / AC-009) **on the dev profile these runs used**. The release-profile runs further down meet it. The
in-process test does not, and that is the point: `TabPool::PAGU_MB` is asserted
inside the test, so `cargo xtask check` stays green while the shipped daemon
breaks the budget by 3-5 MB.

The difference is not the tabs, it is the surface: the daemon adds the axum HTTP
plane, the CDP websocket proxy, the control plane, the handoff wiring, and one
SQLite connection per pool (each with its own page cache). ~45 MB of that sits
outside the pool, which is why the pool-only number looked comfortable.

An earlier reading in `PROGRESS.md` called `MemoryColdStore` "the conservative
case". That was wrong in the direction that matters: keeping snapshots in RAM
costs *less* than the daemon's real cold store plus its HTTP surface, so the
conservative-case label gave a false sense of headroom.

## Bug found while measuring

The first attempt at 99 tabs died at tab 26:

```
Error: createTarget 26: {"code":-32000,"message":"tab pool: penyimpanan dingin gagal:
  \"menyimpan tab 1: database: FOREIGN KEY constraint failed\""}
```

`tab_snapshot.session_id` references `session(id)`, and the daemon never opened a
`session` row: `StoreColdStore` was handed a session id and wrote rows for it
without ever inserting the parent. The unit tests passed because each of them
called `Store::open_session` first, so the obligation was satisfied by the test
harness and by nothing in the production path.

Fixed in `crates/browserd/src/tabstore.rs`: the adapter now opens its own session
on the first `simpan` (`pastikan_sesi`, one `AtomicBool` so it costs one INSERT
per adapter, not one per eviction). A session that already exists is accepted,
which keeps the restart path and the existing tests valid. Regression test:
`tabstore::tests::tab_dingin_tersimpan_walau_tidak_ada_yang_membuka_sesi_dulu`,
which fails on the old code with exactly the FK error above.

This is the class of bug a gate cannot catch on its own: the gate proves the
pool works, not that the daemon wires the pool correctly.

## What is NOT proven

- PSS under `--release`. All numbers here are the dev profile, which allocates
  more and optimises nothing. The release build has not been measured, and until
  it is, these numbers are an upper bound of unknown tightness, not the shipped
  figure.
- PSS with the *hot* tabs holding real remote pages instead of `setDocumentContent`
  documents. Remote pages bring stylesheets, images and scripts, and the CSS
  cascade and raster paths allocate on top of the DOM.
- Whether the daemon's memory returns after the tabs are closed. `idle_reap_secs
  = 0` in this run on purpose, so nothing was reaped; the reap path at 99 tabs is
  a separate measurement.

## Release profile: the ceiling is met, and the gate measures the shipped binary

Measured 2026-11-13 07:20-07:35 WIB. The section above ends with "PSS under
`--release` has not been measured", so every number there was a dev-profile
upper bound of unknown tightness. That gap is now closed, and closing it changed
what the gate has to do.

| Run | Profile | PSS at 99 tabs | Baseline | Targets | Result |
|---|---|---|---|---|---|
| 1 | release | **89.93 MB** | 21.5 MB | 99 | pass |
| 2 | release | **89.3 MB** | 21.4 MB | 99 | pass |
| 3 | release, via the new test | **88.7 MB** | 8.5 MB | 99 | pass |
| 4 | dev, via the new test | 112.7 MB | 22.0 MB | 99 | printed, not enforced |
| 5 | intermediate (`lto = false`, CGU 16) | **109.3 MB** | 9.4 MB | 99 | **fail** |

The release binary meets NFR-001 at the acceptance workload: 88.7-89.9 MB for 99
tabs of 3005-node documents against the 100 MB ceiling. The ~45 MB of surface
outside the pool (axum HTTP plane, CDP websocket proxy, control plane, one SQLite
connection per pool) is real and still there; on release it fits under the
ceiling instead of over it. On dev it does not, because rquickjs is unoptimised,
and that is the compiler's cost, not the daemon's.

### The gate rejected its own first design

The first version enforced the ceiling on an intermediate profile
(`inherits = "release"` with `lto = false`, `codegen-units = 16`) to keep CI
cheap. The gate failed at **109.3 MB** (run 5). That number is not a regression:
without LTO, roughly 20 MB of cross-crate code stops being inlined. So the gate
was measuring a binary that is never shipped, and a gate like that has only two
endings: permanently red, or a ceiling raised until it means nothing. Both are
the failure mode the gate exists to prevent, so the profile was deleted and CI
now runs `--release`.

Run 5 is therefore kept on purpose. It is the proof that the check
**discriminates**: it fails on a binary that misses the budget and passes on the
one that meets it. A test that only ever passes proves nothing about the thing
it claims to guard.

### Where the gate lives

- `crates/browserd/tests/native_daemon_budget.rs` launches `browserd serve` with
  its own keygen'd store and `idle_reap_secs = 0`, fills 99 tabs over CDP, and
  reads `Pss:` from `/proc/<daemon pid>/smaps_rollup`. It compares against
  `tab_pool::PAGU_MB`, not a second constant that could drift from the PRD.
- `.github/workflows/ci.yml` job `budget` runs it with `--release`. It is a
  separate job with its own 60-minute timeout, because the release profile shares
  no artefacts with the dev profile, so its build is cold and roughly as long as
  the whole `gate` job. Keeping it inside `gate` would have turned a cold build
  into a gate timeout that reads like a code failure.
- The dev profile still prints its number and never asserts it. A permanently red
  gate gets ignored by everyone, which is how the pool-only test stayed green
  while the daemon was over budget in the first place.

### The gate ran in CI, 2026-11-13

CI run `43340680688` on commit `ae02432` finished with both jobs green:
`gate (fmt + clippy + test)` and `budget (PSS daemon 99 tab, release)`. The
budget job is the first measurement of the shipping profile on a host that is not
this laptop, and it printed:

```
daemon pid 7134: PSS 70.0 MB (awal 8.9 MB) untuk 99 target, dokumen 3005 node, 99 tab dalam 1.1 detik, pagu 100 MB
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.36s
```

70.0 MB against the same 100 MB ceiling, 18.7 MB below the laptop's 88.7 MB for
the identical workload. The gap is host-side, not a code change: `ae02432` is a
documentation commit, so CI built the same source, and the test and the ceiling
are untouched. PSS divides every shared page by the number of processes mapping
it, so a runner image with a different page cache and linker layout reports a
different share of the same mapping. The laptop figure stays the one to quote,
because it is the larger number on the host where the daemon is developed; the CI
figure is the receipt that the gate passes in the environment that enforces it.

### Still not proven

- PSS with the hot tabs holding real remote pages instead of
  `setDocumentContent` documents. Remote pages bring stylesheets, images and
  scripts, and the CSS cascade and raster paths allocate on top of the DOM.
- Whether the daemon's memory returns after the tabs are closed.
  `idle_reap_secs = 0` in every run here on purpose, so nothing was reaped.
- The 25-hot-tab cap is asserted in the pool-only test, not in this one; this
  test measures the process, so a cap violation would show up as a memory number
  rather than as a named failure.
