---
document_type: project-progress
project: "OpenCrabs Browser: web-opening engine"
prd: "PRD-MESIN-BUKA-WEB 041126-0631.md"
status: in-progress
current_milestone: 5
total_milestones: 5
last_updated: "2026-11-07 07:40"
---

# OpenCrabs Browser Progress

Live state for this target only. The PRD holds the milestone outcomes, Done When,
and the FR/NFR/AC scope; this file records evidence and the next step, it does
not copy the specification.

## Resume Packet (a fresh session reads this first)

One read must be enough to continue without asking the owner anything.

| Field | Value |
|---|---|
| Active milestone | None open. All five PRD milestones are Done (milestone 5 covers both engine adapters). The next unit of work is the owner's call, with the open backlog listed under "Not built, not claimed" below |
| Next exact command | `cargo xtask check` |
| Bootstrap command | `git clone https://github.com/adi805/opencrabs-browser.git && cd opencrabs-browser && cargo build --release --locked` |
| Smoke check | `curl -s http://127.0.0.1:9224/json/version` returns JSON with a non-empty `Browser` field, then `node bench/cdp-smoke.mjs --endpoint http://127.0.0.1:9224` prints 6/6 |
| Last passing check | `cargo xtask check` at `193abc4e` plus the edits in this commit: `fmt ok`, `clippy ok`, `test ok`, exit 0 (2026-11-06) |
| Open blockers | None inside this repo. The three former owner decisions are defaults in PRD section 2 Review Focus. Permanent deletion of `adi805/opencrabs-web` is outside this repo and waits on `gh auth refresh -s delete_repo` |

## Current State

- PRD version: 0.1.0 (implementation)
- Execution mode: native (no build has been requested for new requirements)
- Active milestone / next action: milestones 1 to 5 are **Done** (eyes and
  hands, failure classification + diagnostics, session and engine resilience,
  unified audit, and both engine adapters, `oxi` and `chrome-full`, each with a
  measured capability probe). Audit of 26 AC: **26 VERIFIED, 0 UNVERIFIED**,
  full gate green.
  The two ACs that used to wait for automatic proof are closed: AC-007 (host
  routing to an engine) through an integration test, AC-019 (production port
  9222 isolation) through a Rust test that starts a real engine on a random
  port and then compares against 9222.
- Build authority: the owner granted full execution permission on 3 November 2026.
- Required unresolved input: none. The three owner decisions that used to be
  open questions are now defaults with a reversal cost in PRD section 2 Review Focus.

## Milestones And Evidence

| # | PRD milestone reference | Status | Current evidence / remaining gap |
|---|---|---|---|
| 1 | §4 #1 Eyes and hands | ✅ Done | Runtime `oc-mata.js` + `oc-tangan.js` injected into every new tab (`Page.addScriptToEvaluateOnNewDocument`), and `crates/browserd/tests/mata_tangan.rs` proves it on a real engine: 14 custom checkboxes clicked until page state changed, contents of a `closed` shadow DOM and same-origin iframes read, a click covered by an overlay reported as failing without changing the page. `cargo test -p browserd --test mata_tangan` = 2 passed |
| 2 | §4 #2 Diagnostics + escalation | ✅ Done | `klasifikasi.rs` (6 categories, mandatory evidence, 12 tests), `artefak.rs` (4 files + quota, 6 tests), `eskalasi.rs` (wait -> retry with backoff max 2 -> switch engine -> handoff, 10 tests), host-to-engine routing + `validasi()` of rules at start, metric `browser_open_failures_total{kategori}`. `crates/browserd/tests/diagnostik.rs` proves it on a real engine: normal -> `Terbuka`/200, 403 flagged as a challenge -> `BlokAntiBot`, 2FA -> `ButuhManusia`, dead port -> `Jaringan`. `cargo test -p browserd --test diagnostik` = 1 passed |
| 3 | §4 #3 Session + engine resilience | ✅ Done | `lifecycle::lease` (per-session lease store, zero TTL refused, the limit counted from leases still alive, 11 tests), `browserd::reload` (SIGHUP swaps the `max_tabs` map without a restart; a broken config keeps the old limit, 4 tests), `browserd::anggaran` (per-engine PSS supervisor, recovery promised under 30 seconds and 31 seconds counted as late, 8 tests), `cdp` fail-closed for unknown engines + fail-open for metrics that cannot be read. Live evidence in `crates/browserd/tests/sesi_live.rs` (4 tests): an answered handoff continues the same tab (`window.__sesi` survives), a cancelled handoff releases the tab, SIGHUP changes `max_tabs` 3 -> 1 -> 4 with the pid unchanged, a broken config during reload keeps the old limit. A handoff carries a title, a PNG under `<store_db>/handoff/`, and the `#challenge-form` selector |
| 4 | §4 #4 Unified audit | ✅ Done | Audit of 26 AC in `docs/prd/AUDIT-AC 041126-1718.md`: 24 VERIFIED, 2 UNVERIFIED (AC-007 routing per host, AC-019 production port isolation), both with reasons. Those two were closed in the following round, so it is now 26 VERIFIED, 0 UNVERIFIED. One defect was found and fixed (`9ba1129`): the `--port` default of `browserd` pointed at 9222, the production browser port. Gate in the same tree as code `9ba1129`: fmt rc 0, clippy rc 0 with no warnings, `cargo test --workspace --no-fail-fast` 32 suites ok / 427 passed / 0 failed rc 0, `cargo xtask check` rc 0 |
| 5 | §4 #5 Engine adapters with a measured capability probe | ✅ Done | `crates/engine/src/oxi.rs` behind the shared `Proc`, `penggerak_untuk("oxi")`, and `discover_oxi()`. CI run `42300230272` on commit `0315c95` measured the engine instead of trusting its README: DOM, `Runtime.evaluate`, cookies, and a real PNG came back ok, while page script and input dispatch failed, so `TERUKUR_OXI` keeps `script: false` and `input: false` (`docs/prd/BUKTI-OXI 071126-0519.md`). `crates/engine/src/penuh.rs` runs `google-chrome` headed on an Xvfb display (`-nolisten tcp`, two `Proc`s, a test that refuses any `--headless` in the argument list), `penggerak_untuk("full")` and discovery know it, and `crates/browserd/tests/penuh_probe.rs` measured it on the development laptop: `navigator.webdriver` = `false`, layout, page script, cookies, and dispatched input all ok, boot 3.2 to 11.8 s, about 1.5 GB RSS across 15 to 18 processes. `TERUKUR_FULL` now claims exactly those four (`docs/prd/BUKTI-FULL 071126-0734.md`) |

## Resume And Changes

- 2026-11-04 06:31: PRD created from the 2-4 Nov 2026 conversation and the repo
  evidence (HEAD `f13fb01`, `max_tabs` already enforced; browserd running on 9224
  with `max_tabs = 6`; production 9222 untouched).
- Measured evidence used in the PRD: empty engine 26 MB PSS, 6 tabs 187 MB PSS,
  `--single-process` 3 tabs 522.8 MB versus 858.8 MB, Firefox headless 896 MB idle
  without `/json/version`, Lightpanda zero layout.
- 2026-11-04 07:50 WIB: Milestone 1 finished and proven by live tests, not by
  claims. Five bugs were found by those tests themselves, all of them real:
  (1) axum answered `text/plain` so Chromium wrapped the fixture in `<pre>`;
  (2) `setAttribute` inside a custom element constructor made Chromium
  **permanently** fail to upgrade the element (14 boxes became
  `HTMLUnknownElement`, 0x0 pixels); (3) event constructors and input prototypes
  were taken from the top window realm so elements inside an iframe threw
  `Illegal invocation`; (4) `Node.contains()` does not cross a shadow root
  boundary, so the host was reported as "covered" by its own contents;
  (5) the state snapshot only read the top document, so a successful click inside
  an iframe was reported as "no change".
- Test teardown is now audited: the engine process group must disappear within 5
  seconds after `stop()`, with a `Reaper` that still kills the group if an
  assertion fails midway. Before this, a red run left 15+ ghost browsers behind
  and ate RAM until a reboot.
- 2026-11-04 08:05 WIB: Evidence on the **running** daemon, not just in tests:
  after `install -m755 target/debug/browserd ~/.opencrabs/bin/browserd` and a
  restart of `opencrabs-browserd.service` (9224, production 9222 untouched), an
  ordinary CDP client created a new tab and then got
  `typeof window.__oc === "object"`. `__oc.mata()` saw the button
  (`peran:"button"`, box 43x21, `asal:"dom"`), and `__oc.klik({nama:'Halo'})`
  answered `berubah` through the `klik-native` path while the button text in the
  page really changed to the clicked state.
- Evidence numbers: `cargo test -p browserd --test mata_tangan` 2 passed;
  `cargo test -p browserd --lib` 70 passed.
- What is **not** measured and therefore not claimed: anti-bot detection rate on
  real hosts, `chrome_full` consumption under xvfb, and the effectiveness of the
  tab lease.

- 2026-11-04 16:39 WIB: Milestone 2 finished with evidence, not claims.
  Three sites that previously failed were reopened through daemon 9224 and the
  results changed the classification design (details in
  `docs/prd/BUKTI-BUKA-ULANG 041126-0857.md`): Google answered **429** at
  `/sorry/index` with empty page text so status + URL pattern + reCAPTCHA frame
  became the evidence, `nowsecure.nl` served hCaptcha without a single document
  event because the tab was born already carrying a URL (navigation happened
  before `Network.enable`), and a dead host landed on
  `chrome-error://chromewebdata/` which turned out to be the strongest network
  evidence.
- The code born from that evidence: `klasifikasi.rs` (6 categories, `Bukti` must
  carry a cause and at least one supporting fact, 12 tests), `artefak.rs`
  (4 files per failure plus a quota of 20 attempts / 64 MB, refusing to write
  inside the work tree, 6 tests), `eskalasi.rs` (wait for the network to settle ->
  retry with backoff at most 2 -> switch engine -> handoff, stopping at the first
  step that works, 10 tests), plus `validasi()` in `policy::select` which rejects
  a rule that defaults to an engine that is not installed.
- Last gate numbers: `cargo xtask check` -> fmt ok, clippy ok, test ok;
  `cargo test --workspace` = 15 suites, 0 failed.
- What is **not** yet measured in milestone 2: the effectiveness of
  `stealth_hosts` per host, and the anti-bot detection rate on real hosts other
  than the three sites above.

- 2026-11-04 11:54 WIB: Milestone 3 finished, and three of its four paths were
  born from the same failure: a daemon that has to be shut down to change one
  number is a daemon that will be shut down at the wrong time.
- Lease holds back reap (AC-009, AC-012, AC-013). `lifecycle::lease` keeps pure
  in-memory leases with injected time, so the expiry rule is tested without
  waiting. Three decisions define its behaviour: `pegang` rejects a zero TTL (a
  lease without a deadline cannot be told apart from a leak), the per-session
  limit is counted from leases still ALIVE rather than the lifetime total, and
  `ganti_batas` does not revoke leases that already exist. `Lease::hidup` is
  inclusive at the expiry second so that "15 minutes" means 15 minutes. Reap
  evidence:
  `crates/browserd/tests/idle_reap.rs::lease_hidup_menahan_reap_dan_kedaluwarsa_tidak`.
- SIGHUP swaps limits without a restart (AC-010, AC-011). `browserd::reload`
  loads the config, validates it, then swaps the `max_tabs` map inside the
  `EnginePool` shared by every clone (`Arc<RwLock<..>>`), and `loop_reap` re-reads
  `idle_reap` on every 5 second tick so SIGHUP can TURN ON a reap that was
  previously disabled. A broken config changes nothing: the old limit is kept and
  the reason is recorded, it does not fall back to no limit. `--idle-reap-secs`
  from the CLI still wins even after a reload, because that flag is an operator
  instruction for this process.
- The memory budget restores the engine, it does not kill the daemon (AC-016,
  AC-017). The supervisor reads PSS per process group (not RSS), and when the
  hard threshold is crossed the engine is stopped and started again through
  `spawn_blocking` while the daemon keeps serving. The 30 second promise is
  recorded honestly: a 31 second recovery is counted as LATE through the
  `pulih_terlambat` counter, not as a success. A failed measurement is left open
  with a warning, because a supervisor that kills engines because it cannot read
  `/proc` is more dangerous than no supervisor at all. A `[budget]` that only
  writes part of a threshold is rejected with the name of the missing section and
  key, not accepted with a made-up value.
- The failure direction is chosen deliberately (AC-020): unknown destination =
  fail-CLOSED (error -32602 naming the requested engine; guessing another engine
  would send the command to a tab the caller does not own), metrics that cannot
  be read = fail-OPEN (rejecting new tabs only because metrics could not be read
  would kill a healthy daemon).
- A human handoff carries context that is ready to answer (AC-014): the title
  through `Runtime.evaluate` capped at 200 characters, a real PNG screenshot
  under `<store_db>/handoff/` with a file name naming the second and the
  sanitised target id (an id containing `/` must not be able to write outside
  that directory), and a selector only for expressions that `JS_DETECT` actually
  looks for; a text signal answers `None` rather than inventing a selector.
  Base64 is decoded in house without a new dependency because invalid input must
  become an error, not a broken PNG displayed as evidence. Every fetch fails
  open: the handoff is still created without the field that failed, and a
  screenshot file that ends up unreferenced is removed again.
- Live evidence in `crates/browserd/tests/sesi_live.rs` (4 tests, 2.91 seconds):
  `handoff_dijawab_melanjutkan_tab_yang_sama` proves `window.__sesi` survives a
  handoff through to the operator's answer (the SAME tab, not a new one),
  `handoff_dibatalkan_melepas_tab_dan_mesin_siap_lagi`,
  `sighup_mengubah_batas_tab_tanpa_restart` (3 -> 1 -> 4 with the pid unchanged),
  and `config_rusak_saat_reload_mempertahankan_batas_lama`.
- One real bug was found by the full gate, not by this milestone:
  `engine::slot::pastikan` wrote `terakhir` with a `sekarang` taken BEFORE the
  engine was started, so an engine that had just finished booting looked idle for
  its entire boot time and could be reaped right after being used. Fixed in
  `fix(engine)` with a test driving a 1.2 second boot.
- One live test was flaky for a while, and its first cause was real:
  `m4_proof::dua_mesin_nyata_tidak_saling_mengganggu` read `document.title` once
  the moment `frameNavigated` arrived, even though that event marks a COMMIT
  navigation, not a parsed document. Replaced with a retry bounded at 5 seconds.
- That retry was not enough, and the failure was not a single one. Under
  `cargo test --workspace` three different tests failed in the same shape
  ("CDP command not answered", `Connection reset without closing handshake`)
  while each passed on its own (`m4_proof` waiting for `Page.enable` 90 seconds,
  `mata_tangan` waiting for `Runtime.evaluate` 10 seconds, `sesi_live` finding no
  screenshot). The daemon log saved on failure pointed at the cause: engine
  `kiri` stopped at `06:11:09` although it was born at `06:11:04`, meaning the
  process was killed, not slow to answer.
- The killer was `earlyoom`. Since 12:00 it had recorded 221 `chrome-headless`
  kills and 71 `opencrabs` kills (the latter being what made the agent session
  restart repeatedly in the middle of the gate). The trigger was
  `oc-serve-embed.service` (the OpenCrabs embedding server) spinning in a restart
  loop: `NRestarts=641`, `Restart=always`, each cycle loading a 200-800 MB model
  and then getting SIGTERM when swap hit 0%. That loop squeezed host memory, and
  earlyoom picked its next victim from the largest process currently running:
  the engine under test.
- Two steps, because there were two causes. First, the live test memory footprint
  was reduced: live tests are now serialised through a host-wide `flock` in
  `crates/browserd/tests/common/mod.rs`, held for the duration of one live test,
  so it is no longer five binaries spawning Chromium at the same time. The lock
  is `std::fs::File::try_lock` (Rust 1.98): zero new dependencies, and the kernel
  releases it if the process dies so there is no stale lock. The lock file lives
  in the build's own `target` directory so other checkouts on the same host do
  not queue behind it. Second, the `oc-serve-embed` loop was reported to the
  owner as a host problem, not patched from the test side.
- Gate numbers after both: `cargo fmt --all -- --check` rc 0,
  `cargo clippy --workspace --all-targets -- -D warnings` rc 0, then
  `cargo test --workspace --no-fail-fast`: 32 suites ok, 426 passed, 0 failed,
  rc 0, and **delta 0 earlyoom kills** during the run. Before this, the same run
  with serialisation active but the host still mid-spiral failed three targets;
  so this green came from healthy memory, not from luck.

- What is **not** yet measured in milestone 3, and not claimed: (1)
  `Gudang::pegang` is NOT called from any production path yet; lease versus reap
  is proven in tests (`idle_reap.rs`), but no daemon code actually holds a lease
  on behalf of a session. (2) The 900 second handoff TTL is tested with a fake
  clock in `control::registry`, not live. (3) `loop_anggaran` is not tested
  end-to-end with a real engine; recovery is proven with a fake engine.
### Milestone 4: unified audit of 26 AC

- The audit of every FR/NFR/AC is in `docs/prd/AUDIT-AC 041126-1718.md`, built
  on HEAD `7dffb6c`. The status rule is single, and that is what makes the number
  trustworthy: VERIFIED means there is a command someone else can re-run and it
  passes at that gate; everything else is written UNVERIFIED with its reason.
  Result: **24 VERIFIED, 2 UNVERIFIED**. Both were closed after that round, so
  the current number is in the Host routing and Production port isolation
  sections below.
- The two that were not raised were not under-checked, their evidence simply did
  not exist. AC-007 (host routing to an engine) had complete and unit-tested
  logic, but zero callers outside `crates/policy/src/select.rs`; target creation
  always used the default engine (`crates/cdp/src/targets.rs:270`). AC-019
  (production port isolation) evidence was still a manual procedure: `/json/list`
  on 9222 showed 0 targets before and after a live test, while the live test
  itself really did start Chromium on the dev port.
- The audit found one real defect, and it was the least pleasant kind: code and
  its comment contradicted each other. `browserd serve` without a flag and
  without a config file would try to bind 9222, the production browser port, even
  though the comment above that argument had claimed 9224 all along. This stayed
  hidden because the systemd unit passes an explicit `--bind`, so what had been
  protecting us was the config file, not a correct default. Fixed in `9ba1129`,
  plus a locking test `default_port_adalah_9224_bukan_port_produksi`.
- Gate numbers in the same tree as code `9ba1129`: `cargo fmt --all -- --check`
  rc 0, `cargo clippy --workspace --all-targets -- -D warnings` rc 0 with 0
  warning lines, `cargo test --workspace --no-fail-fast` 32 suites ok, 0 FAILED
  suites, 427 passed, 0 failed, rc 0, then `cargo xtask check` rc 0 (fmt ok,
  clippy ok, test ok). The first fmt run was red on `crates/browserd/src/cli.rs`
  because the new test module was not formatted yet; the shape was fixed, then
  the whole gate was run again. The gate was not loosened and no allowed-warning
  was added.
- What is **not** yet measured in milestone 4, and not claimed: three items from
  milestone 3 still apply unchanged (`Gudang::pegang` not called from a production
  path, the 900 second handoff TTL still a fake clock, `loop_anggaran` not
  end-to-end with a real engine), plus two new ones: AC-007 and AC-019 had no
  automatic proof yet. This gate run also did not measure memory at start and end,
  so there is no claim at all about earlyoom kills in this round.

### Re-run gate: reap must not run before the threshold

- The re-run gate at HEAD `03fca2e` surfaced a real regression, and the regression
  sat on a production path, not in a test: `cargo test --workspace --no-fail-fast`
  exited 101 with `panicked at crates/browserd/tests/idle_proof.rs:343: reap
  happened before the threshold: 9.852608104s < 10 s`.
- The root cause was a single place that truncated a unit. `unix_seconds()`
  (`crates/browserd/src/daemon.rs:327`) rounded timestamps down to the second,
  `Slot.terakhir` stored seconds, and `diam()` computed
  `Duration::from_secs(sekarang - terakhir)`. Two timestamps both rounded down
  make the difference **overstate** idle time by up to a full second, so reap can
  run one second before its threshold. A test threshold of 10 seconds means reap
  may run at 9 seconds; a production threshold of 60 seconds
  (`crates/browserd/src/config.rs:344`) means an engine that has been idle for
  only 59 seconds can be shut down.
- The fix is millisecond resolution, not a looser threshold: `detik_sekarang()`
  became `ms_sekarang()` in `crates/engine/src/slot.rs`, `as_millis` and
  `Duration::from_millis` replaced `as_secs` and `from_secs`, then the change was
  propagated to `crates/cdp/src/lib.rs`, `crates/cdp/src/engines.rs`,
  `crates/browserd/src/wiring.rs`, and `crates/browserd/src/daemon.rs`. The
  daemon now has two clocks with two units and that is deliberate: `loop_reap`
  uses milliseconds, while the handoff TTL and the lease stay in seconds, with
  the reason written in the code. The remaining wrong direction is now reversed
  into the safe one: reap is at most a millisecond late, never a second early.
- That the result is not luck was proven by repetition, not a single run:
  `idle_proof` ran 3 times in a row, 1 passed 0 failed each run (55.95 s / 16.36 s
  / 15.60 s), and `idle_reap` ran 3 times in a row, 6 passed 0 failed each run.
  Full gate in the same tree: `cargo fmt --all -- --check` rc 0,
  `cargo clippy --workspace --all-targets -- -D warnings` rc 0 with 0 warning
  lines, `cargo test --workspace --no-fail-fast` rc 0 with 32 suites ok, 427
  passed, 0 failed, 0 ignored, then `cargo xtask check` rc 0 (fmt ok, clippy ok,
  test ok).
- The gate was not loosened and that is deliberate: the reap tick stays 5
  seconds, the test threshold stays 10 seconds, no new allowed-warning. The only
  addition is a 250 ms measurement slack in the test, with its direction and
  reason written in the code (the "last used" timestamp is written by the daemon
  before its response reaches the test's stopwatch, so the stopwatch always
  starts slightly late). That number is bracketed from both sides: far above
  localhost latency and far below the 5 second reap tick, so no early reap can
  slip through that gap.

### Host routing to a connected engine on the real path (AC-007)

- The initial finding was not a code bug, it was a missing caller:
  `policy::select`, `rule_for`, and `host_matches` in
  `crates/policy/src/select.rs` had zero callers outside their own file, and
  target creation always fell back to the default engine. The rule logic was
  correct, but not one real path read it.
- Three connections were wired, one per layer. In `policy` a
  `select_untuk_host(host, &PolicySection, &Catalog)` was added that returns
  `Ok(None)` when no rule matches, plus 4 unit tests. In `cdp` the
  `PemilihMesin` contract and `host_dari_url` were added, then the
  `Target.createTarget` branch in `crates/cdp/src/mux/peta.rs` reads the rule
  when a client does not name an engine explicitly. In `browserd` there is
  `crates/browserd/src/routing.rs` holding `PemilihPolicy`, which wraps
  `PolicySection` + the engine catalog, and `crates/browserd/src/daemon.rs`
  wires it through `ProxyState::pool(..).with_observer(..).with_pemilih(..)`.
- Three properties are deliberately preserved because they are the core of
  AC-007. An explicit client request (`openCrabsOptions.engine`) wins over a host
  rule; a kind that a rule picks but that is not installed is answered with
  `-32000` naming the kind, never silently replaced by the default engine; and
  the rule that won is recorded through `tracing::info!` so it can be read from
  the log, not merely correct in a test.
- Evidence at two levels. `cargo test -p browserd --test routing_host` = 4
  passed, running through a real axum proxy with two fake websocket engines of
  different kinds: rule `toko.test -> chrome-full` lands on the `full` instance,
  an explicit request lands on the engine the client asked for, a kind that is not
  installed is answered with `-32000`, and zero rules uses the default engine.
  Unit tests in `crates/cdp/src/mux/peta/uji.rs` lock the rejection path and the
  assignment path at the mux level without needing a socket.
- One possibility deliberately left open: `/json/new` always uses the default
  engine, because that endpoint carries no URL that could be mapped to a host. If
  it is needed later, the path is an explicit host parameter, not a guess from a
  header.
- Audit status changed: AC-007 from UNVERIFIED to VERIFIED with a command anyone
  can re-run.

### Production port isolation becomes an automatic test (AC-019)

- AC-019 was the only one still UNVERIFIED, and the reason was not missing code
  but evidence that was only manual: in the previous round isolation was measured
  with `/json/list` on 9222 before and after a live test, recorded as a table in
  the audit document, with no command anyone else could repeat.
- It is now `crates/browserd/tests/isolasi_port.rs`. The chain is the same as the
  hand procedure: read the target count on 9222, start `browserd serve` on a
  random port with a temporary profile, create one tab so the engine really
  exists, then read 9222 again and compare. The engine debug port is read from
  the `DevToolsActivePort` file the engine writes itself, not from an expected
  number.
- One hard rule is held: 9222 is only read. If the production browser is using
  that port, that browser must not be disturbed, so what is compared is its state,
  not its contents. A change from `None` to `Some` is still caught, because a
  development daemon grabbing 9222 would open a listener there.
- The `Resolved::resolve` chain is locked at unit level first, four tests in
  `crates/browserd/src/daemon.rs`: the default bind can never end up on `:9222`,
  `[daemon]` without `bind` stays 9224, 9222 is only reachable through a `bind`
  written by the operator, and the default engine profile lives in
  `.cache/opencrabs-browser`, separate from the 9222 profile. Unit tests stop a
  regression quickly; the live test stops the regression that only shows up when
  a real engine starts.
- Evidence: `cargo test -p browserd --lib daemon::tests` = 16 passed, 0 failed;
  `cargo test -p browserd --test isolasi_port` = 1 passed, 0 failed, exit 0. The
  live test uses a real `chrome-headless-shell`, the engine got port 37107 and
  not 9222, and it is skipped with a clear message when the engine binary is
  missing.
- The full gate ran in one pass (`cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo xtask check`,
  `cargo test --workspace --no-fail-fast`): all exit 0, 450 tests passed, 0
  failed, 0 panic, 34 suites 'test result: ok'. No gate was loosened and no new
  allowed-warning was added.
- Audit status changed: AC-019 from UNVERIFIED to VERIFIED. `docs/prd/AUDIT-AC`
  is now 26 VERIFIED, 0 UNVERIFIED.


### Milestone 5: the `oxi` adapter, and the process rule it forced out

- The question milestone 5 asks is not "can we add another engine" but "does the
  boundary we built in M4 survive a second implementation". OxiBrowser is a
  Rust browser with a CDP server, so it is the honest test: a different binary,
  a different CLI, a different `/json/version` shape, the same daemon.
- The blocker came before any code. Every OxiBrowser release asset is a Mach-O
  universal binary for macOS: `file` on `oxibrowser-unsigned` from
  `v0.28.0` says "Mach-O universal binary with 2 architectures", `sha256`
  `ed60eef1b80455308e3bf3a24ec34029d0ed66c1508329818a6be58e470461a2`. There is no
  Linux build to download, so the engine is compiled from crates.io: 722
  packages, Blitz, Servo, boa, BoringSSL. That build does not happen on a 4 core
  laptop, so it happens in `.github/workflows/oxi-probe.yml`.
- `crates/engine/src/proses.rs` is the piece that came out of writing the second
  adapter. The M1 rule (SIGTERM to the whole group, wait 5 seconds, SIGKILL) lived
  inside `ShellEngine`; a second engine that copied it would have drifted, so it
  moved into one `Proc` with `spawn` (always `process_group(0)`), `is_running`,
  `kill_group`, and `stop`. `ShellEngine` now calls it, `OxiEngine` now calls it,
  and two tests prove the group is gone by `killpg(pgid, None) == ESRCH` instead
  of trusting our own flag.
- `crates/engine/src/oxi.rs` exists because OxiBrowser speaks CDP with different
  keys. Its `/json/version` returns `browser`, `protocolVersion`, `userAgent`,
  `webkitVersion`, `webSocketDebuggerUrl` in camelCase, while Chromium returns
  `Browser`, `User-Agent`, `Protocol-Version`. `info_dari_json` accepts both
  spellings and the test locks both, because a boot that fails on a lowercase
  letter is a boot that fails.
- The engine also refuses to talk to a private address by default (SSRF guard),
  so the adapter passes `--allow-private-ips` only when the caller asks for it in
  `extra_args`. The guard is never disabled inside the adapter; that decision
  stays with whoever writes the config, where it can be read.
- `penggerak_untuk(kind)` in `crates/engine/src/slot.rs` replaces the `match` that
  only knew `shell`. `daemon.rs` no longer bails with "only 'shell'": `--engine`
  accepts `oxi`, `dari_config` accepts `kind = "oxi"`, and `pencarian_biner`
  picks the right discovery path (`discover_shell` or `discover_oxi`, the latter
  reading `OXI_BIN`, `~/.cargo/bin/oxibrowser`, `~/.local/bin/oxibrowser`).
  `shell_binary` and `shell_args` stay gated to `kind == "shell"` so an oxi
  config cannot be booted with Chromium flags.
- `crates/policy/src/select.rs` stops pretending. `Catalog::dari_mesin` used to
  report every capability as `true` for every recognized kind, which is a guess
  dressed as a capability table. It now reads named constants: `TERUKUR_SHELL`
  (measured), `TERUKUR_FULL` (no adapter, so `false`), and `TERUKUR_OXI`
  (measured by the probe, see below). The honest default for an unmeasured
  engine is `false`.
- The measurement that closed M5: run `42300230272` on commit `0315c95` compiled
  OxiBrowser 0.28.0 on ubuntu-latest and ran
  `cargo test -p browserd --test oxi_probe -- --nocapture`. Thirteen checks came
  back `ok` (`/json/version`, `Target.createTarget`, `Target.attachToTarget`,
  `DOM.getDocument`, `Runtime.evaluate` = `42`, `Page.navigate`,
  `document.title` = `Halaman Probe Oxi`, `Page.captureScreenshot` = real PNG of
  6188 base64 characters, `Network.setCookie` read back unchanged). Two came
  back `GAGAL`: page scripts never execute (`document.body.dataset.skrip` empty)
  and `Input.dispatchMouseEvent` never reaches the page (`#hasil` stayed
  `belum`). The input probe installs its listener over CDP, so its failure is
  not a side effect of the script failure. Table and raw output:
  `docs/prd/BUKTI-OXI 071126-0519.md`.
- `TERUKUR_OXI` now carries those numbers: `layout: true`, `script: false`,
  `cookies: true`, `input: false`. A request that needs `script` or `input` is
  therefore refused for engine `oxi` at selection time, before a job starts
  rather than halfway through it.
- The same run proved the daemon side end to end:
  `daemon_melayani_mesin_oxi_dari_config` and
  `oxi_bicara_cdp_ke_halaman_nyata_lalu_mati_bersih` both `ok`
  (`crates/browserd/tests/oxi_live.rs`), the second ending with a process group
  that is gone, checked by `killpg(pgid, None) == ESRCH` instead of a flag we
  set ourselves.

### Milestone 5, second half: `chrome-full` inside xvfb

- This half was deferred in the PRD for one reason: a full browser was said
  to be "not measured on this 7 GB laptop", and building it on an unmeasured
  machine is how you end up with an engine nobody can run. The owner asked for
  it on 2026-11-07, and the measurement came first, not the claim.
- `crates/engine/src/penuh.rs` is two processes with one life. Xvfb is started
  with `-nolisten tcp -noreset` on the first display whose socket **and** lock
  are both free, then `google-chrome` is started with `DISPLAY` pointing at it.
  Both go through the shared `Proc`, so the stop path is the same SIGTERM ->
  5 second grace -> SIGKILL to the whole process group that every other engine
  uses. If either step fails, everything already alive is killed before the
  error is returned: an Xvfb left behind by a failed boot holds a display and its
  memory until the next reboot and never shows up in a log.
- The one flag that must never appear is `--headless`, and that is enforced by a
  test rather than by care: with it, the engine is a `chrome-headless-shell`
  with extra steps and the entire reason this adapter exists disappears. The same
  file locks two other facts: `-nolisten tcp` (an X server listening on TCP is an
  open door) and that caller-supplied arguments are appended last so they can
  override a default.
- `crates/engine/src/probe.rs` gained `discover_full()` and `discover_xvfb()`,
  both reading an override (`CHROME_FULL_BIN`, `XVFB_BIN`) before the usual
  paths. `penggerak_untuk("full")` in `slot.rs` and `pencarian_biner` in
  `daemon.rs` route the kind to the adapter, so `kind = "full"` in a config or
  `--engine full` on the CLI boots this engine and not a shell with a different
  name.
- The measurement is `crates/browserd/tests/penuh_probe.rs`, and it exists to
  answer one question the PRD could not: does a real browser on a virtual
  display actually behave differently, or is that a story? It does.
  `navigator.webdriver` is the boolean `false` here, where the shell reports
  `true`. Beyond that: the fixture loads over HTTP, the title and an element box
  (1200x600) are read, the screenshot is a real PNG of 12820 base64 characters,
  the page's own script runs (`dataset.skrip` = `ya`), a cookie survives a write
  and read, and a dispatched mouse event changes the DOM. The probe also proves
  teardown: after `stop()` the engine is gone and the X display is gone.
- Boot time on this host measured 3.2 to 11.8 seconds and about 1.5 GB RSS across
  15 to 18 processes. One run crossed the 30 second ready timeout, and the cause
  was not the adapter: 36 browser instances leaked by earlier test suites
  (`sesi_live`, `m4_proof`, `diagnostik`, `idle_proof`) were still alive,
  reparented to `systemd --user`, holding 4.9 GB between them. Killing them
  brought boot back to 3.2 seconds. The probe now waits 60 seconds instead of 30,
  and the number it reports is still the measured time, not the ceiling.
- `TERUKUR_FULL` in `crates/policy/src/select.rs` now says `true` for `layout`,
  `script`, `cookies`, and `input`, and the doc comment on it names the probe and
  the evidence file. Nothing beyond that table is claimed: the anti-bot detection
  rate on a real host, memory with six tabs, and stability over hours were not
  measured here, so they are not in any document as facts.

## Not built, not claimed

Open items, each with the reason it is still open and what would close it.

| Item | Why it is open | What would close it |
|---|---|---|
| `cloakbrowser` artifact path (the cookie handoff named in `crates/policy/src/challenge.rs:304`) | It was never part of the `chrome-full` adapter, and no test drives it. A path that reads another browser's cookie store has to be measured against a real challenge, not written against a guess. | A probe that takes a cookie jar produced by a real challenge and proves the cookie is usable through the daemon, with the file format locked by a test |
| Leaked browsers from the live test suites | 36 orphaned `chrome-headless-shell` processes were found alive during this milestone, 4.9 GB RSS between them, from `sesi_live`, `m4_proof`, `diagnostik`, and `idle_proof`. The serialisation lock in `tests/common` limits how many start at once, it does not stop them surviving a killed run. | A teardown that is verified after a test is killed mid-run, not only after it returns, plus a check in `xtask check` that the process count returns to its starting value |
| `xtask e2e` | The command is referenced in the task list but does not exist yet | An `e2e` subcommand that runs the live suites in the same serialised way `cargo test` does |
| Permanent deletion of `adi805/opencrabs-web` | Outside this repo, and it needs a token scope the host does not have | `gh auth refresh -s delete_repo`, then the delete, with the local backup at `~/oc-archive/opencrabs-web.git` (HEAD `2f22714`) kept until one clone from it succeeds |

## AI Agent Rules

Read the PRD once per session, then the files that changed. Do not repeat
discovery that has not changed, do not claim VERIFIED without tool output, and do
not treat this PRD as permission to build.