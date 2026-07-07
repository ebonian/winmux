# Follow-ups from the MVP final review (2026-07-07)

Ticketed by the final whole-branch review of the multiplexing MVP (branch
`feature/multiplexing-mvp`), then resolved or refined across the
server/client split (sub-project 2, branch `feature/server-client-sessions`).
None ever blocked a merge.

1. **RESOLVED** (sub-project 2, Task 6). Dead panes retained their `Pty`
   until closed. Fixed: the server drops `Pty` at `Event::Exited`
   (`PaneRuntime.pty = None`), and a natural pane exit now auto-removes the
   pane entirely (tmux `remain-on-exit off` default, wired up in Task 7).
2. **STILL OPEN** — deliberate choice. Confirm race when `Ctrl-b x y` arrive
   in one stdin read: without care the `y` could be forwarded to the shell
   before confirm mode arms. The server/client design fixes this
   *structurally* rather than closing the race in the MVP's single-batch
   sense: server-side input is dispatched **one event at a time** against
   live confirm state (see the `## server contract` discussion and the
   module docs in `src/server.rs`), so a batch of stdin bytes can no longer
   race arming. This is intentionally left listed as open rather than
   "resolved" because it changes the mechanism (event-at-a-time dispatch)
   rather than eliminating the underlying class of race in the abstract —
   see `src/server.rs` module docs for the exact reasoning before assuming
   it's closed for good.
3. **RESOLVED** (sub-project 2, Task 8, commit `09a274a`). `Host::enter()`
   had a partial-failure gap: code pages/stdout mode were mutated before the
   `RESTORE` snapshot was published. Fixed by publishing `RESTORE` before the
   first mutation.
4. **PARTIALLY RESOLVED** (sub-project 2, Task 6). Unbounded event-channel
   growth under pane output flood: events are now drained and coalesced once
   per main-loop turn before a single render, instead of one render per 4 KB
   `Output` chunk. The underlying `mpsc` channel itself is still unbounded —
   a sufficiently fast, never-drained producer can still grow the queue
   without limit. Residual risk, not eliminated.
5. **RESOLVED** (sub-project 2, Task 10). `layout`'s Right/Down adjacency
   checks in `focus_dir` computed `f.x + f.w + 1` / `f.y + f.h + 1` with
   plain `+`, which is a theoretical `u16` overflow (debug-mode panic) for
   areas near `u16::MAX`. Fixed with `saturating_add`; TDD tests
   `focus_dir_right_near_u16_max_does_not_overflow` and
   `focus_dir_down_near_u16_max_does_not_overflow` in `src/layout.rs`
   construct such areas and assert no panic.
6. **RESOLVED** (sub-project 2, Task 10). `grid::cell()`'s panic message
   lacked coordinates, hurting debuggability. Fixed: message is now
   `cell(<col>, <row>) out of bounds <cols>x<rows>`, e.g. `cell(90, 5) out of
   bounds 80x24`; TDD test `cell_panic_message_includes_coordinates_and_dimensions`
   in `src/grid.rs` pins the exact text with `#[should_panic(expected = ...)]`.

## New follow-ups from the server/client split (sub-project 2 reviews)

Non-blocking minor debt accumulated across the sub-project 2 plan's task
reviews. None affect the sub-project 2 merge.

7. **`pty::win_err` doesn't unmask the HRESULT.** `src/pty.rs`'s `win_err`
   does `io::Error::from_raw_os_error(e.code().0)`, passing the raw
   (negative, HRESULT-shaped) `i32` through. `src/pipe.rs`'s `win_err` goes
   through `raw_win32_code`, which unmasks HRESULTs built from Win32 codes
   (`FACILITY_WIN32`) back to the plain Win32 error number before wrapping,
   so `.kind()` classification (e.g. `ErrorKind::NotFound`) works correctly.
   `pty.rs` currently never branches on `.kind()`, so this is latent, not
   active — but if `pty` code ever starts matching on error kind, backport
   `pipe.rs`'s unmasking for consistency.
8. **`pipe.rs` username buffer is 256 `u16`s, not `UNLEN + 1`.**
   `current_username()` uses a fixed `[0u16; 256]` buffer with `GetUserNameW`.
   Windows' documented `UNLEN` is 256, so the correct buffer size is
   `UNLEN + 1` (257, for the trailing NUL) — the current buffer is one
   `u16` short of the documented worst case for a maximum-length username.
   Practically unreachable (real usernames are far shorter), but worth
   sizing to the documented constant instead of a round number.
9. **`protocol::write_frame` doesn't itself enforce `MAX_FRAME`** — only
   `read_frame` rejects an oversized declared length on decode. Verified the
   actual bound on the write side: `src/server.rs`'s `send_output` chunks
   pane `Output` bytes via `bytes.chunks(protocol::MAX_FRAME as usize)`
   before calling `write_frame`, and the pane reader in `src/server.rs`
   reads at most 4096 bytes (`[0u8; 4096]`) per `ReadFile`, i.e. every
   `Output` frame today is bounded by the 4 KiB pane read buffer, far under
   the 1 MiB `MAX_FRAME`. There is currently no other producer that could
   hand `write_frame` an oversized payload, so this is latent: a future
   caller passing a payload over `MAX_FRAME` would silently write a
   wire-format-violating frame (the receiver's `read_frame` would then
   correctly reject it, but only after the sender has already written
   invalid bytes to the pipe). Consider an assert or defensive chunk/error in
   `write_frame` itself.
10. **Trailing payload bytes are silently ignored by protocol decoders.**
    `read_client_msg`/`read_server_msg` parse known fields out of the
    frame's payload slice via sequential `read_*` helpers but never check
    that the slice is fully consumed afterward. A frame with correct known
    fields plus extra trailing bytes (e.g. a newer client talking to an
    older server, or a corrupted length) decodes successfully today instead
    of erroring. Low risk while client/server ship from the same binary, but
    would matter for any future wire compatibility story.
11. **Kill-last-pane-of-window-via-`x` cascade in a multi-window session is
    untested.** `server_proto.rs` covers `kill_only_pane_confirm_destroys_session`
    (single-window session) and window-kill via `&` (`kill_window_confirm_text`),
    but there is no test that kills the last pane of a *non-last* window (via
    `Ctrl-b x` confirm) in a session that has other windows, to confirm the
    window is destroyed and focus/selection lands correctly on a remaining
    window without disturbing the rest of the session.
12. **`drain_after_exit`'s 10×50 ms poll heuristic** (`tests/common/mod.rs`)
    is a fixed-iteration sleep loop rather than a condition-based wait. It
    has been reliable in practice but is inherently a timing guess; a
    genuinely slow CI box could still flake it. Consider a bounded
    `wait_until`-style predicate loop instead if it ever becomes flaky.
13. **Unbounded per-client writer channel.** Per the server/client design
    (`docs/specs/2026-07-07-server-client-design.md`, "Server architecture"),
    each client's writer thread drains an unbounded `mpsc<Vec<u8>>` by
    design, so a slow/stalled client can never block the main loop — but the
    same unboundedness means a client that reads slower than the server
    produces output (e.g. a frozen SSH session that hasn't dropped yet) can
    grow that queue's memory without limit. Bounded per the same tradeoff
    reasoning as follow-up #4; not addressed here.

## Accepted debt from the final whole-branch review (2026-07-07)

Ticketed by the final review of `feature/server-client-sessions` (all 10
plan tasks complete; review verdict "ready to merge with fixes" — the fixes
are the control-char name validation covered elsewhere in this review round,
tracked separately from the items below, which are accepted debt rather than
merge blockers).

14. **Main-loop pane-input writes can block all sessions on one stalled
    pane.** `InputEvent::Forward` -> `pty.write_input` runs inline on the
    server's single main-loop thread, unlike pane *output* and per-client
    writes (both already off the main loop via dedicated threads/channels,
    see follow-up #13). A pane whose child process stops draining stdin (a
    hung app, or a huge paste) blocks `write_input`, which blocks the main
    loop, which blocks rendering and input for EVERY session on the server,
    not just the stalled pane's. Structural fix (a per-pane writer channel +
    thread, mirroring the existing per-client writer design) planned as a
    fast-follow; becomes more urgent once sub-project 3 adds `send-keys`
    (a scripted/automated way to pump arbitrary-sized input at a pane).
15. **Named-pipe ACL relies on the default DACL.** `CreateNamedPipeW(...,
    None)` (in `src/pipe.rs`) passes no explicit `SECURITY_ATTRIBUTES`, so
    the pipe gets Windows' default DACL. Combined with per-username pipe
    naming (so a different user's pipe would need to be guessed), this is
    low-risk in practice: the default DACL grants Everyone read-only connect
    at most, which isn't sufficient to speak the client/server protocol
    usefully. Still, an explicit owner-only `SECURITY_ATTRIBUTES` would be a
    more defensible posture than relying on the platform default; consider
    for a later hardening pass.

### Review ticket batch (final review, 2026-07-07) — short one-liners, none blocking

16. `client.rs`'s stdin-reader thread panicking leaves the main loop waiting
    on it forever instead of signaling the main loop to exit non-zero.
17. `attach -d` (steal) evicted clients get a bare `[detached]` exit message
    (`src/server.rs:509`); tmux says `[detached (from session <name>)]`.
    When fixed, also update the `## server contract` table's documented
    exit-message string in `docs/specs/2026-07-07-server-client-interfaces.md`.
18. `destroy_session`'s `TerminateProcess` loop over a session's panes runs
    sequentially on the main thread; bounded by pane count so not a real
    scaling concern today, but worth a comment noting the assumption.
19. `kill-server` accept race: a client that connects during the server's
    teardown window sees `[lost server]` (`src/client.rs:156`) rather than
    the cleaner `no server running on <pipe>` (`src/main.rs:177`) a client
    connecting slightly later would get.
20. `src/input.rs`'s `set_capture(true)` doc comment overstates how much
    state it clears; the `capture_mode_clears_pending_prefix_state` test
    name similarly overpromises relative to what it actually asserts.
21. `src/pipe.rs`'s accept loop has a dead `Ok(())` arm; `finish_attach`
    does a redundant `Renderer::new` immediately followed by a `resize`
    that makes the `new` call's initial size irrelevant.
22. Untested paths: `rename_session` propagation to OTHER attached clients
    (not just the renaming one); `SwitchClientPrev` as a no-op when only one
    session exists; `list-windows`' default-target-most-recent-session
    behavior with no `-t`; the kill-last-pane-via-`Ctrl-b x` cascade in a
    multi-window session (see also follow-up #11, which is the same gap
    phrased for the `&`/window-kill path).
23. CLI unit tests assert against the parsed `cmd` field only, not the full
    `Invocation`; `unknown_flag_err` only asserts the error string is
    non-empty rather than pinning its exact text; `-x 0`/`-y 0` is treated
    as a "use default size" sentinel rather than a literal zero size, which
    isn't obvious from the test names alone.
24. `no_console_fails_fast` test naming is inconsistent with the sibling
    tests around it (naming convention drift, not a behavior issue).

## Deferred from sub-project 3 (command layer + config, 2026-07-07) — SP4 candidates

Documented deviations from real tmux, accepted for SP3's scope (global-only
options, one dispatcher shared by all four entry points) and carried forward
as sub-project 4 ("parity polish") candidates rather than merge blockers.

25. **`escape-time` is parsed and stored but not honored.** The option
    exists in the registry (`src/options.rs`) with tmux's default (500ms)
    and round-trips through `set`/`show`, but nothing reads it back to
    govern the actual Escape-vs-Alt-sequence disambiguation window in
    `src/keys.rs`'s input decoder or `src/input.rs`'s `KeyMachine` — that
    timing is currently a fixed constant, not configurable.
26. **No per-session/per-window option scopes.** `Options` (Task 4) is one
    global instance on `Server`, matching tmux's `-g` (global) scope only;
    real tmux allows session- and window-level overlays (`set -w`,
    unprefixed `set` inside a window context) that override the global
    value. SP3 accepts `-g`/bare `set` as globally-scoped regardless of the
    flag actually passed, which is a real behavioral gap for any config that
    relies on per-window styling.
27. **Format engine covers a fixed subset of `#`-codes, not the general
    tmux format language.** `expand_format` (`src/options.rs`) supports
    `#S`/`#W`/`#I`/`#P`/`#F`/`#H`/strftime-style `%H:%M`-class codes and
    nothing else — no `#{...}` braced expressions, no conditionals
    (`#{?...}`), no arithmetic/string format functions. `status-right`'s
    real tmux default (`#{=21:pane_title}`-bearing) is out of reach for this
    reason (documented deviation in `src/options.rs`'s `default_value`).
28. **`automatic-rename` is inert.** The option is registered with tmux's
    default (`on`) and round-trips through `set`/`show`, but no code path
    actually renames a window based on its running command — window names
    only ever change via explicit `rename-window`/the `,` prompt/config.
29. **`status-interval` is stored but unused for a general refresh timer.**
    The status-right clock still only re-renders on a minute-granularity
    change-detector (`server.rs`'s `local_clock`/Tick handling, inherited
    from SP2), not on the configured interval — a custom `status-right`
    format with sub-minute-sensitive content (were the format engine to
    support one, see #27) would not refresh on schedule.
30. **`bind -n` (no-prefix bindings) can't be given a bare printable
    character.** The `-n` (root-table, no-prefix-required) binding path
    exists and is tested against non-printable/special keys, but tmux's
    real semantics — binding a bare letter with `-n` shadows normal typing
    in that pane entirely — is not exercised or specifically guarded against
    for printable characters; SP3's key-machine dispatch order for that case
    is unverified.
31. **`status-right`'s inline `#[...]` per-segment style overrides are not
    parsed.** Real tmux lets `status-right`/`status-left` embed
    `#[fg=red,bold]`-style directives mid-string to change color partway
    through the line; SP3 renders the whole right side in one style
    (`options.status_style()`, per `render_one`'s `right_style: base`) — see
    the code comment at `src/server.rs`'s status-row assembly, "status-right
    styling via `#[]` inline styles is SP4; until then the right side is
    drawn with the row's base style."
