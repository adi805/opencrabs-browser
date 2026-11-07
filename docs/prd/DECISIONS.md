---
document_type: project-decision-log
project: "OpenCrabs Browser: web-opening engine"
status: active
created: "2026-11-04"
---

# OpenCrabs Browser Decision Log

Source of truth for product and architecture decisions on this target. PRD toolkit
policy is not recorded here.

## Decisions

- [2026-11-07 07:34] Decision: the `chrome-full` adapter runs a real browser and patches nothing
  Status: Accepted
  Approval: owner ("Mulai milestone 6 chrome-full")
  Requirement IDs: FR-004 (routing to an engine by host rule)
  Context: the cheap way to make `navigator.webdriver` false is
    `--disable-blink-features=AutomationControlled` plus a list of spoofed
    properties. Every one of those flags changes engine behaviour for a claim
    nobody here has measured.
  Rationale: running Chromium headed on an Xvfb display already gives
    `navigator.webdriver` = `false` and a desktop user agent, measured by
    `crates/browserd/tests/penuh_probe.rs`. Fingerprint patching buys an
    unmeasured advantage at the cost of a browser that no longer behaves like
    the browser it claims to be.
  Impact: `PENUH_FLAGS` in `crates/engine/src/penuh.rs` contains no
    automation-hiding flag. If one is ever needed, it arrives with a measurement
    that shows what it changes, not with a story about detection.

- [2026-11-04 07:0x] Decision: the primary goal is eyes and hands in the browser
  Status: Accepted
  Approval: owner (translated from the Indonesian original: "You have hands, eyes, like that, in everything in this browser you can see and click everything")
  Requirement IDs: FR-008, FR-009, FR-010
  Context: the obstacle that came up most often was not "the browser does not run",
    it was that I could not see and click certain elements (custom elements, shadow
    DOM, iframes, buttons that can only be clicked at a coordinate).
  Rationale: without complete perception and action, failure classification only
    tells me why I failed, it does not make me able to succeed.
  Impact: milestone 1 changed from "diagnostics" to "eyes and hands";
    diagnostics and escalation moved down to milestone 2.

- [2026-11-02 04:33] Decision: `max_tabs` is enforced at the protocol layer
  Status: Accepted
  Approval: owner ("Enforce max_tabs")
  Requirement IDs: FR-005 (limit), NFR-001 (memory fence)
  Context: the config already named `max_tabs` but it was only read and then
    ignored, so operators believed a limit existed when it did not.
  Rationale: the number of record is read from the engine (`/json/list`), not from
    a daemon counter; the rejection message names the number and the way out.
  Impact: `/json/new` answers 429, `Target.createTarget` answers `-32010`, and a
    rejected command never reaches the engine.

- [2026-11-04 06:31] Decision: the render engine stays Chromium, light engines are only the text path
  Status: Accepted
  Approval: measured evidence (not preference)
  Requirement IDs: FR-001, FR-002
  Context: Lightpanda saves memory but has zero layout engine; Firefox headless has
    no `/json/version` and sits at 896 MB idle.
  Rationale: an unstyled screenshot and a `getBoundingClientRect()` that throws
    make that engine unusable for visual work.
  Impact: replacement engine candidates left the scope; the savings come from
    lifecycle (reap, budget, tab limit), not from swapping the engine.

- [2026-11-04 06:31] Decision: `max_tabs` and `idle_reap_secs` must take effect through SIGHUP
  Status: Proposed
  Approval: none yet (waiting for the owner)
  Requirement IDs: FR-005, AC-010, AC-011
  Context: today both numbers are read once at start, so changing them means
    restarting the daemon and cutting a session that is still running.
  Rationale: a reload that parses first and swaps second avoids a daemon dying
    because of a half-written config.
  Impact: `EnginePool` needs a limit that can be updated while alive; the systemd
    unit needs `ExecReload`.

- [2026-11-04 06:31] Decision: tab sessions use a lease, not one reused tab
  Status: Proposed
  Approval: none yet (waiting for the owner)
  Requirement IDs: FR-005, FR-006
  Context: the agent manager reuses one tab, so parallel work overwrites itself and
    `max_tabs` is never felt.
  Rationale: a lease gives an auditable limit and lets reap know which tabs are
    still in use.
  Impact: needs a second piece of state (a per-session lease) that must have a TTL
    so it cannot leak memory.

- [2026-11-07 08:17] Decision: a dropped `Proc` kills its process group
  Status: Accepted
  Approval: none needed; this is the teardown defect that made the milestone 6
    probe fail after it had passed once
  Requirement IDs: NFR-003
  Context: `Proc` wrapped a `Child` and had no `Drop`, so a panic between launch
    and `stop()` left Chrome and Xvfb alive. One failed probe left a 222 MB
    Chrome tree and an Xvfb display behind, and the next run timed out at the
    20 second CDP ceiling because of it.
  Rationale: the process group is already the unit that must die, and the drop
    path covers every exit including a panic, where no explicit `stop()` will
    ever run. `stop()` remains the graceful path so the profile and its cookies
    are closed properly first.
  Impact: dropping an engine now terminates it. A caller that wants a graceful
    shutdown still has to call `stop()` before the value goes out of scope.

- [2026-11-07 08:52] Decision: live test suites close the daemon with SIGTERM, not `Child::kill()`
  Status: Accepted
  Approval: none needed; this is the teardown defect that made a full gate go red
    on unchanged code
  Requirement IDs: NFR-003
  Context: five suites called `Child::kill()`, which sends SIGKILL, so the
    daemon's graceful shutdown never ran and its engines were orphaned. A gate
    failed in `sesi_live` with a 15 second boot timeout after 26 such machines
    had accumulated, and `penuh_probe` was racing those suites because it did
    not take the existing `kunci_live` lock.
  Rationale: the daemon already stops its engines correctly on SIGTERM, which
    was verified by reading the shutdown path before changing the helpers. The
    escalation to SIGKILL stays as a ceiling, so a hung daemon still cannot hang
    the suite.
  Impact: live tests now take up to a few seconds longer per daemon on a slow
    host, and leave no engines behind. A test binary killed from outside still
    runs no destructor; that gap needs a process census in `xtask check`.

## AI Agent Rules

A decision with status Proposed must not be treated as permission to build. An
Accepted one may be cited as a basis, and a Superseded one is kept, not deleted.