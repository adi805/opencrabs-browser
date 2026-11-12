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

Both daemon runs **exceed the 100 MB ceiling** (NFR-001 / AC-009). The
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
