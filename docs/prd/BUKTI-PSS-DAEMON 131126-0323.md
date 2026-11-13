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

## CI re-measurement after the text commits: the gate went red, then closed

**Closed on 2026-11-13 by `2e3a1bf`.** The cause was a per-tab `LaporanLayout`
retained in `TabPanas` and the fix is below ("Cause: measured" through "Result"),
where CI job `budget` reads 62.9 MB. The rest of this section is the state as
measured before that fix: it is kept because it is the red half of the
discrimination proof, not because it still describes the tree.

Measured by CI job `budget`, not by this laptop. The section above ends with the
gate green at 70.0 MB in CI on `ae02432`. That was the state before the text
commits, and any record that still quotes 88.7-89.9 MB as "under the ceiling,
enforced in CI" is stale.

| CI run | Commit | Budget job | PSS at 99 tabs | Baseline |
|---|---|---|---|---|
| 43340680688 | `ae02432` | pass | 70.0 MB | 8.9 MB |
| 43342364963 | `11ebfd6` | pass | not recorded here | n/a |
| 43405324790 | `3614fcd` | **fail** | **107.7 MB** | 9.1 MB |
| 43406098635 | `5e687cb` | **fail** | **106.3 MB** | 9.3 MB |
| 43411910557 | `1e55054` | **fail** | **105.3 MB** | 9.1 MB |
| 43415375465 | `c61cbd0` | **fail** | **110.6 MB** | 9.3 MB |

This is the gate doing its job, not a flake: four consecutive commits, four
different daemon pids, every reading between 105.3 and 110.6 MB against the same
100 MB ceiling. The panic is at
`crates/browserd/tests/native_daemon_budget.rs:417`:

```
PSS daemon 110.6 MB melewati pagu 100 MB: daemon pid 7350: PSS 110.6 MB (awal 9.3 MB) untuk 99 target, dokumen 3005 node, 99 tab dalam 1.4 detik, pagu 100 MB
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.61s
```

The laptop figure of 88.7-89.9 MB does not contradict these runs, because it was
taken at 07:20-07:35 WIB and the first text commit is 11:49 WIB. Both figures are
honest; only the claim that the current tree is under the ceiling is false.

### What changed between the green run and the red ones

`11ebfd6` (08:06 WIB) is the last commit whose CI was green. Two code commits
land between it and the first red run, both in the native engine, both about
text:

- `a23e77a` (11:49 WIB) `feat(engine): measure native text and create layout
  boxes` adds a 759 720-byte `DejaVuSans.ttf` to the binary, a text measurement
  module, and three per-document maps in `crates/engine/src/native/layout.rs`:
  `gaya` (`HashMap<NodeId, Vec<(String, String)>>`), `terhitung`
  (`HashMap<NodeId, GayaTerhitung>`) and `teks` (`HashMap<NodeId, String>`).
- `bcff1ea` (16:56 WIB) `feat(engine): paint native text glyphs in the raster`
  adds glyph outlines and the painting path.

### Cause: measured, and it was the retained layout

The arithmetic pointed at the three per-node maps. The measurement says the maps
cost a fraction of the gap, and the real owner was a different object: every hot
tab carried a whole `LaporanLayout` that nothing in the production path read.

Two numbers, both from tests that count allocation instead of PSS, so they are
deterministic and do not move with host load:

| What | Test | Bytes per tab | MB per tab |
|---|---|---|---|
| `LaporanLayout` alone | `crates/engine/tests/laporan_retensi.rs` | 577 930 | 0.551 |
| DOM arena alone | `crates/engine/tests/dom_retensi.rs` | 677 614 | 0.646 |

They are bounds on two different documents of the same 3005-node size, so they are
not added together. `dom_retensi` builds 1500 short-text rows and its own layout
comes to 205 194 B; `laporan_retensi` builds the budget workload the CI gate uses,
and that layout is the 577 930 B above. The DOM arena is what a tab legitimately
retains. The layout was the part with no reader.

Inside the report, the split on the budget workload (1500 `div`, 3005 nodes) is
`kotak` 98 304 B (3004 boxes), `gaya` 144 344 B (1504 entries), `terhitung`
258 048 B (3007 entries), `teks` 77 234 B (1500 entries). So 25 hot tabs pinned
13.8 MB of a value no reader asked for.

Allocator fragmentation is ruled out rather than assumed away.
`crates/browserd/tests/native_99_tabs_trim.rs` opens 99 tabs in process and then
calls `malloc_trim(0)`:

```
awal 2.4 MB -> 99 tab 41.9 MB (+39.5) -> trim 41.6 MB (allocator mengembalikan 0.3 MB, trim=true)
```

0.3 MB comes back, so the rest is referenced data. That is what sent the search
to the owner of the data instead of to malloc tuning.

### The fix

`crates/engine/src/native/tab_pool.rs` stopped storing a layout per tab
(`2e3a1bf`). Layout is a pure function of `(document, viewport)`, and its only
production reader is the paint layer, which recomputes it in
`paint::lukis_dengan`. `TabPool::viewport` and `dengan_viewport` existed only to
feed that stored copy, so they went with it.

Nothing was loosened. `PAGU_MB` stays 100.0, the hot cap stays 25, and the gate
still runs on the release profile in CI job `budget`.

### Result: green in CI, with the number

| CI run | Commit | Budget job | PSS at 99 tabs | Baseline |
|---|---|---|---|---|
| 43460811394 | `21901cc` (fix) | **pass** | **62.9 MB** | 9.3 MB |
| 43447587347 | `62264bb` (same code as the red run) | pass | 95.9 MB | 9.2 MB |
| 43415375465 | `c61cbd0` (pre-fix) | fail | 110.6 MB | 9.3 MB |

The post-fix figure is 62.9 MB against a 100 MB ceiling: 37 MB of headroom.

### One thing this section got wrong, recorded rather than hidden

The paragraph above used to say "this is the gate doing its job, not a flake".
Half of that was wrong. `62264bb` is a docs-only commit whose tree is identical
to `c61cbd0` in every `.rs`, `.toml` and `.yml` file (verified with
`git diff --name-only c61cbd0..62264bb`, which lists three `docs/prd` files and
nothing else). The same code measured 110.6 MB and 95.9 MB on two different
runners, so the pre-fix gate was flaky inside a ~15 MB band that straddled the
ceiling. Four red runs in a row was a real regression, but the ceiling was being
approached, not blown past by a stable margin.

That matters for reading the fix: 62.9 MB sits 37 MB below the ceiling, outside
that band, so the green is not a lucky draw from the same distribution the red
runs came from. It is a different number because it is a different program.

### What is still not proven

- PSS with the hot tabs holding real remote pages instead of
  `setDocumentContent` documents.
- Whether the daemon returns memory after tabs are closed. `idle_reap_secs = 0`
  in every run here on purpose.
- Why 95.9 and 110.6 MB differed on identical code. Runner page cache and linker
  layout are the candidates, and neither was measured.
