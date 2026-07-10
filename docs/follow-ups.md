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
   **Update (sub-project 4, Task 9 fix round):** the `Tick` handler now
   batches every ready client's escape-time flush into one
   `handle_event(Tick)` call (previously each `ServerEvent` touched exactly
   one client's `Stdin`). This slightly widens this same accepted race's
   blast radius: a session/window dying mid-batch from one client's flushed
   event can now affect a second, already-collected-but-not-yet-processed
   client's flush in the same tick. Not a new class of bug, same accepted
   limitation, just a marginally larger surface — no behavior change made.
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

7. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10). `pty::win_err`
   now unmasks the HRESULT back to the raw Win32 code via a `raw_win32_code`
   helper duplicated from `src/pipe.rs`'s (same shape, two independent pure
   modules), matching `pipe.rs`'s existing convention. Test:
   `pty::tests::win_err_unmasks_hresult_to_plain_win32_code`.
   *Original text:* `src/pty.rs`'s `win_err`
   does `io::Error::from_raw_os_error(e.code().0)`, passing the raw
   (negative, HRESULT-shaped) `i32` through. `src/pipe.rs`'s `win_err` goes
   through `raw_win32_code`, which unmasks HRESULTs built from Win32 codes
   (`FACILITY_WIN32`) back to the plain Win32 error number before wrapping,
   so `.kind()` classification (e.g. `ErrorKind::NotFound`) works correctly.
   `pty.rs` currently never branches on `.kind()`, so this is latent, not
   active — but if `pty` code ever starts matching on error kind, backport
   `pipe.rs`'s unmasking for consistency.
8. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10).
   `current_username`'s buffer is now sized `UNLEN + 1` (257 `u16`s) via a
   named `const UNLEN: usize = 256`, not a bare `256`. Test:
   `pipe::tests::unlen_plus_one_is_257`.
   *Original text:* `pipe.rs` username buffer is 256 `u16`s, not `UNLEN + 1`.
   `current_username()` uses a fixed `[0u16; 256]` buffer with `GetUserNameW`.
   Windows' documented `UNLEN` is 256, so the correct buffer size is
   `UNLEN + 1` (257, for the trailing NUL) — the current buffer is one
   `u16` short of the documented worst case for a maximum-length username.
   Practically unreachable (real usernames are far shorter), but worth
   sizing to the documented constant instead of a round number.
9. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10).
   `protocol::write_frame` now rejects (`Err(InvalidData)`) any payload over
   `MAX_FRAME` itself, rather than only `read_frame` catching an oversized
   declared length on the receiving end. Tests:
   `protocol::tests::write_frame_rejects_oversized_payload`,
   `write_frame_accepts_exactly_max_frame`.
   *Original text:* `protocol::write_frame` doesn't itself enforce `MAX_FRAME`
   — only
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
10. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10). Every decoder
    that parses a fixed set of known fields (`Attach`/`Resize`/`Detach`/`Cli`
    on the client side, `Exit`/`CliDone` on the server side) now calls a new
    `expect_consumed` helper after its last `read_*` call, erroring
    `InvalidData` on any leftover bytes. `Stdin`/`Output`, whose entire
    payload IS the value with no fixed-field structure, are unaffected (there
    is nothing to be "trailing" relative to). Tests:
    `protocol::tests::decoder_rejects_trailing_bytes_{attach,resize,exit,cli,detach}`.
    *Original text:* Trailing payload bytes are silently ignored by protocol
    decoders.
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

14. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10). `PaneRuntime`
    gains a per-pane `input_tx: Sender<Vec<u8>>` writer channel; `spawn_pane`
    also spawns a dedicated writer thread (owning an independent duplicate of
    the pty's input handle via the new `Pty::try_clone_writer`) that drains
    it, mirroring the existing per-client `spawn_writer` design exactly. The
    main loop's hot Forward/Key-forwarding path now enqueues onto this
    channel instead of calling `pty.write_input` inline — a stalled pane's
    writer thread can block, but never the main loop, never another session.
    `src/server/dispatch.rs`'s lower-volume write sites (send-keys,
    paste-buffer, mouse-drag forwarding) are unchanged, still direct
    `pty.write_input` calls — only the two hottest call sites moved. Contract
    amendments: `2026-07-06-mvp-interfaces.md`'s `## pty` section
    (`Pty::try_clone_writer`/`PtyWriter`) and
    `2026-07-07-server-client-interfaces.md`'s `## server` section
    (architecture note). Test:
    `tests/server_proto.rs::stalled_pane_stdin_does_not_block_other_sessions`
    (RED against the pre-fix inline call, ~3.8s; GREEN after, ~1s).
    *Original text:* Main-loop pane-input writes can block all sessions on
    one stalled
    pane. `InputEvent::Forward` -> `pty.write_input` runs inline on the
    server's single main-loop thread, unlike pane *output* and per-client
    writes (both already off the main loop via dedicated threads/channels,
    see follow-up #13). A pane whose child process stops draining stdin (a
    hung app, or a huge paste) blocks `write_input`, which blocks the main
    loop, which blocks rendering and input for EVERY session on the server,
    not just the stalled pane's. Structural fix (a per-pane writer channel +
    thread, mirroring the existing per-client writer design) planned as a
    fast-follow; becomes more urgent once sub-project 3 adds `send-keys`
    (a scripted/automated way to pump arbitrary-sized input at a pane).
15. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10). Every named
    pipe instance (`create_instance`, `src/pipe.rs`) is now created with an
    explicit `SECURITY_ATTRIBUTES` (`OwnerDacl`) granting the current
    process's owner SID `GENERIC_ALL` and nothing else, built via
    `OpenProcessToken`/`GetTokenInformation(TokenUser)` +
    `InitializeAcl`/`AddAccessAllowedAce`/`InitializeSecurityDescriptor`/
    `SetSecurityDescriptorDacl`, in place of the platform default (`None`).
    Behavior otherwise identical; covered by the full existing
    `tests/pipe_smoke.rs` suite (all pipe creation/connect paths go through
    `create_instance`).
    *Original text:* Named-pipe ACL relies on the default DACL.
    `CreateNamedPipeW(...,
    None)` (in `src/pipe.rs`) passes no explicit `SECURITY_ATTRIBUTES`, so
    the pipe gets Windows' default DACL. Combined with per-username pipe
    naming (so a different user's pipe would need to be guessed), this is
    low-risk in practice: the default DACL grants Everyone read-only connect
    at most, which isn't sufficient to speak the client/server protocol
    usefully. Still, an explicit owner-only `SECURITY_ATTRIBUTES` would be a
    more defensible posture than relying on the platform default; consider
    for a later hardening pass.

### Review ticket batch (final review, 2026-07-07) — short one-liners, none blocking

16. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10). The stdin
    thread's body now runs inside `std::panic::catch_unwind`; a panic sends
    an `Err` through the same channel the reader thread uses (it holds its
    own `tx` clone), which the main loop's existing fatal-error handling
    already turns into a clean non-zero exit instead of hanging forever.
    *Original text:* `client.rs`'s stdin-reader thread panicking leaves the
    main loop waiting
    on it forever instead of signaling the main loop to exit non-zero.
17. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10).
    `detach_others` now sends `Exit{0, "[detached (from session
    <name>)]"}`, identical text to every other detach exit path in this
    module. Contract amendment:
    `2026-07-07-server-client-interfaces.md`'s `## server` "Attach" bullet.
    Test: `tests/server_proto.rs::steal_attach_eviction_message_names_session`
    (RED against the pre-fix bare `[detached]`, GREEN after).
    *Original text:* `attach -d` (steal) evicted clients get a bare
    `[detached]` exit message
    (`src/server.rs:509`); tmux says `[detached (from session <name>)]`.
    When fixed, also update the `## server contract` table's documented
    exit-message string in `docs/specs/2026-07-07-server-client-interfaces.md`.
18. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10). Comment added
    on `destroy_session` noting the sequential-on-main-thread assumption and
    why it's not a real scaling concern (bounded by one session's pane
    count; unlike follow-up #14's stalled-CHILD concern, killing an
    already-alive process isn't something a hung child can meaningfully
    stall).
    *Original text:* `destroy_session`'s `TerminateProcess` loop over a
    session's panes runs
    sequentially on the main thread; bounded by pane count so not a real
    scaling concern today, but worth a comment noting the assumption.
19. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10).
    `client::attach`'s main loop now tracks whether it has ever received a
    real `ServerMsg` on this connection; if the connection is lost before
    that (the exact race this ticket describes — connected and `Attach`
    sent, but the server tore itself down before serving even one reply),
    it prints the cleaner `no server running on <pipe>` (same text
    `main.rs`'s `report_connect_error` uses) instead of the vaguer `[lost
    server]`, which is now reserved for a disconnect AFTER a real session
    was genuinely live. Exit code unchanged (1 either way).
    *Original text:* `kill-server` accept race: a client that connects
    during the server's
    teardown window sees `[lost server]` (`src/client.rs:156`) rather than
    the cleaner `no server running on <pipe>` (`src/main.rs:177`) a client
    connecting slightly later would get.
20. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10). Note: the
    exact test this ticket named (`capture_mode_clears_pending_prefix_state`)
    belonged to the sub-project 2 `InputMachine`, deleted when sub-project 3
    Task 6 rewired `src/input.rs` onto the table-driven `KeyMachine` — its
    replacement, `capture_bypasses_prefix`, already existed under a
    different name by the time this ticket was picked up. Closed here by (a)
    splitting `KeyMachine`'s type-level doc comment into precise ON/OFF
    clauses (turning capture ON leaves armed `Prefixed`/repeat-window state
    untouched; only turning OFF resets it) and (b) renaming the test again,
    to `capture_bypasses_prefix_and_off_clears_prefix_state`, which states
    both halves of what it actually asserts.
    *Original text:* `src/input.rs`'s `set_capture(true)` doc comment
    overstates how much
    state it clears; the `capture_mode_clears_pending_prefix_state` test
    name similarly overpromises relative to what it actually asserts.
21. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10). The dead
    `Ok(())` arm in `PipeListener::accept`'s `ConnectNamedPipe` match is
    replaced with an `unreachable!()` arm plus a comment explaining WHY it's
    unreachable (Win32's documented contract: an overlapped-mode
    `ConnectNamedPipe` never returns success synchronously). Separately,
    `finish_attach`'s `Renderer::new(cols, rows)` followed immediately by a
    same-size `resize` (whose only OTHER job, forcing `force_full: true`,
    doesn't need a same-size `new` first) is now `Renderer::new(0, 0)` +
    `resize(cols, rows)`, avoiding the double buffer allocation.
    *Original text:* `src/pipe.rs`'s accept loop has a dead `Ok(())` arm;
    `finish_attach`
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
24. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10). Renamed to
    `e2e_no_console_fails_fast`, matching every sibling test in
    `tests/e2e_sessions.rs` (all `e2e_`-prefixed). Doc reference updated in
    `2026-07-07-server-client-interfaces.md`.
    *Original text:* `no_console_fails_fast` test naming is inconsistent
    with the sibling
    tests around it (naming convention drift, not a behavior issue).

## Deferred from sub-project 3 (command layer + config, 2026-07-07) — SP4 candidates

Documented deviations from real tmux, accepted for SP3's scope (global-only
options, one dispatcher shared by all four entry points) and carried forward
as sub-project 4 ("parity polish") candidates rather than merge blockers.

25. **RESOLVED** (sub-project 4, Task 9). `escape-time` is parsed and
    stored but not honored. The option exists in the registry
    (`src/options.rs`) with tmux's default (500ms) and round-trips through
    `set`/`show`, but nothing reads it back to govern the actual
    Escape-vs-Alt-sequence disambiguation window in `src/keys.rs`'s input
    decoder or `src/input.rs`'s `KeyMachine` — that timing is currently a
    fixed constant, not configurable. Now wired end to end: `KeyDecoder::
    pending_starts_with_escape` + `KeyMachine::set_escape_time`/
    `escape_ready`/`flush_now`, driven by the server's `Tick` handler — see
    `docs/specs/2026-07-07-parity-polish-interfaces.md`'s `## naming`
    section.
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
28. **RESOLVED** (sub-project 4, Task 9). `automatic-rename` is inert. The
    option is registered with tmux's default (`on`) and round-trips through
    `set`/`show`, but no code path actually renames a window based on its
    running command — window names only ever change via explicit
    `rename-window`/the `,` prompt/config. Now wired: a window's active
    pane's OSC title (`grid::Grid`'s Task 1 capture) drives
    `Server::maybe_auto_rename`, gated by the global option AND a new
    per-window `model::Window::auto_rename` flag (permanently cleared by
    any manual rename) — see the `## naming` section referenced above.
    Documented divergence (unchanged from the design spec): the name
    derives from the console title (ConPTY surfaces `SetConsoleTitle` as
    OSC 0), not the foreground process, and `allow-rename`/ESC k remains
    deferred (item still tracked implicitly via the design spec's
    "Documented deferrals" list, not separately itemized here).
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
32. **`pane-base-index` is accepted + stored but inert.** The design spec
    lists it as a real option (default 0) alongside `base-index`, but
    nothing consumes it: pane indexes in kill-pane prompts/targets are
    position-based from 0 regardless of this option's value. Unlike the
    other inert options, it DOES have a typed getter
    (`Options::pane_base_index`) — the getter itself is simply never called.
    Reclassified as accepted-inert (final whole-branch review, 2026-07-07);
    no code change, since it is already stored/validated exactly like the
    other inert options.
33. **Config errors surface via `server.log` + a first-attach transient
    message, not tmux's interactive error view.** Real tmux reports a
    `.tmux.conf` parse/apply error interactively (an in-place message in the
    client that loaded it, with `set -g @tmux_error` style follow-up
    tooling in some configs); winmux instead logs the error to
    `%LOCALAPPDATA%\winmux\server.log` and surfaces a one-shot transient
    message to the first client that attaches after the failed load. This is
    one of the design spec's explicit documented deviations from tmux
    (`docs/specs/2026-07-07-command-config-design.md`'s "Explicit
    deviations from tmux" section) that had not yet been cross-referenced
    here.

## Discovered during sub-project 4 (parity polish)

34. **`Key{code: KeyCode::Space}` is unreachable by a real spacebar
    keypress** (discovered implementing Task 3, selection + paste buffers).
    `keys::classify_single_byte` (the live input decoder) only ever produces
    the `Space` code variant for `Ctrl-Space` (byte `0x00`, explicitly
    special-cased); an ordinary bare spacebar press (byte `0x20`) decodes to
    `Key{code: KeyCode::Char(' ')}` instead. `KeyCode::Space` otherwise
    exists purely for `parse_key("Space")`/`send-keys Space`/`key_name`
    notation (config files, `list-keys` output), not live keyboard input.
    Consequence: ANY default or user binding registered via
    `named("Space")`/`bind ... Space ...` targeting the ROOT, PREFIX, or a
    copy-mode table is unreachable by an actual spacebar press — confirmed
    live via Task 2's pre-existing emacs `copy-mode` default `Space →
    copy-page-down` (`src/bindings.rs`). BOTH default-table cases are now
    fixed at the bindings level: Task 3's own `copy-mode-vi` `Space →
    copy-begin-selection` was written around the gap from the start (bound
    under `Char(' ')`, not `named("Space")`), and the Task 3 REVIEW FIX
    rebound the emacs `Space → copy-page-down` default under `Char(' ')`
    too. What remains open here is only the general decoder-level gap
    (a USER's `bind ... Space ...` config line still silently produces an
    unreachable binding). A real fix belongs in `keys::classify_single_byte`
    (making a
    bare `0x20` decode as `KeyCode::Space` instead of `Char(' ')`, or
    equivalently normalizing `Char(' ')` to `Space` wherever keys are
    looked up) — deferred since it's a decoder-level change with unknown
    blast radius across every existing table/test that relies on today's
    `Char(' ')` semantics for plain-space forwarding to a pane (see
    `input::is_plain_forwardable`, which currently forwards `Char(' ')` as
    ordinary typed input — changing the decode would need to preserve that).

35. **Mouse clicks/drags are never forwarded to the pane application's own
    mouse-reporting mode** (Task 5, mouse). tmux, when a pane's own
    application has ALSO requested mouse reporting (e.g. vim/tmux-inside-
    tmux/htop with mouse support enabled), can forward click/drag events to
    that application instead of consuming them for pane focus/copy-mode/
    resize. winmux v1 always consumes a mouse event for its OWN routing
    (`server::dispatch::dispatch_mouse`) and never re-encodes/forwards the
    raw SGR bytes to the focused pane's pty — explicitly out of scope per
    the task brief ("Click also forwards to the application if the pane's
    program enabled mouse reporting itself — OUT OF SCOPE for this task
    unless the design spec says otherwise" — it doesn't). A real fix would
    need per-pane mouse-mode tracking (has THIS pane's own app requested
    `?1000h`/etc. via its own output stream?) plus a policy for which takes
    priority when both winmux and the inner app want the same click.
36. **Drag-to-select only starts inside a pane already in THAT client's copy
    mode** (Task 5, mouse). Real tmux implicitly enters copy mode on a
    `Drag1` over a LIVE (non-copy-mode) pane; winmux's `mouse_down` only arms
    a selection drag when the click lands inside the pane already bound to
    the client's `ClientMode::Copy` — a plain click-drag on a live pane just
    focuses on `Down` and does nothing further. Matches the design spec's
    own bulleted overview, which scopes "Drag1 = selection" under "In copy
    mode:" specifically; documented as a deliberate v1 scope decision, not
    an oversight.
37. **`word-separators` is a hardcoded constant, not a real tmux option**
    (Task 5, mouse). Double-click word selection (`select_word_at` in
    `src/server/dispatch.rs`) uses tmux's DEFAULT `word-separators` value
    (`" -_@"`) as a private `const WORD_SEPARATORS`, matching the task
    brief's explicit instruction ("hardcode tmux's default set unless the
    option already exists"). A `set -g word-separators <chars>` line is
    accepted/stored nowhere and has no effect — no `word-separators` entry
    exists in `src/options.rs`'s `SPECS` table at all.
38. **Mouse-during-prompt/confirm/choose-tree/display-panes is swallowed
    entirely, not forwarded to the overlay** (Task 5, mouse; scope widened
    in the final SP4 review's merge-gate fix round). `dispatch_mouse` drops
    (silent no-op) any mouse event while the acting client's `ClientMode` is
    `ConfirmCmd`, `Prompt`, `ChooseTree`, or `DisplayPanes`, rather than
    e.g. letting a click land on the pane underneath or interacting with the
    overlay's own content in any way. The task brief left tmux's own real
    behavior here undecided ("decide per tmux and document"); winmux's
    choice prioritizes never letting a stray click race a confirm's y/n
    capture, or hit-test against pane geometry an overlay is currently
    hiding, over any interactive mouse behavior during these (rare,
    short-lived) modal states. `ChooseTree`/`DisplayPanes` originally
    shipped (Task 8) without joining this guard at all — a real bug, not a
    documented deviation, fixed in the same review round; see
    `docs/follow-ups.md` #61 for the follow-on "real tmux-style mouse
    routing into choose-tree" ticket this fix deferred.
39. **Status-row click hit-testing doesn't replicate the renderer's final
    spatial truncation** (Task 5, mouse). `mouse_status_click` rebuilds the
    same left-prefix + per-window-tab span layout `render_one`/
    `status::status_spans` draws to hit-test which window a click column
    belongs to, but does NOT replicate `render::compose_back`'s LAST step —
    right-truncating when the built left+right strings don't fit the actual
    terminal width. On an extremely narrow terminal (narrower than the
    status content), a click past the true truncation point could resolve
    to a window tab that isn't actually visible there. Low practical impact
    (`status-left-length`/`status-right-length` already cap the common case;
    this only matters on terminals narrower than those caps plus every
    window tab combined).
40. **Corner-cell border hit-testing tie-break is arbitrary** (Task 5,
    mouse). `server::dispatch::hit_test` checks vertical-border positions
    before horizontal-border positions, so the single cell at a 4-way "+"
    junction between four panes always resolves to a vertical-border drag,
    never horizontal, with no way for the user to pick the other axis at
    that exact cell (they can still grab a non-corner cell along the
    horizontal border one column over). Documented, not treated as a bug —
    real tmux has the same class of single-cell ambiguity at a "+" junction
    and doesn't document a resolution rule either.
41. **`swap-pane -s`/`-t` cannot move a pane between windows or sessions**
    (Task 6, layout presets). `exec_swap_pane`'s explicit-target form now
    ERRORS (`"swap-pane: can only swap panes within the same window"`)
    rather than silently no-opping when `-s`/`-t` resolve to different
    windows — but real tmux actually supports this (moving a pane to a
    different window/session, swapping it there). Implementing it for real
    would mean teaching `Layout` to remove a leaf from one tree and insert it
    into another (today `Layout::swap_panes` only relabels leaf values within
    a single tree) — worth doing for full tmux parity, but out of scope for
    the Task 6 fix round, which only closed the "silent no-op" gap with an
    honest error.
42. **`swap-pane -U`/`-D` combined with `-s` is rejected, not implemented**
    (Task 6, layout presets). Real tmux's full `swap-pane [-dDU] [-s
    src-pane] [-t dst-pane]` semantics let `-s` additionally override which
    pane a directional (`-U`/`-D`) swap is computed relative to. The Task 6
    fix round implements the more common case (`-t` selects which pane is
    swapped up/down, defaulting to the active pane when `-t` is absent) but
    rejects `-U`/`-D` combined with `-s` with a usage error rather than
    guessing at the full matrix. Worth revisiting if a real workflow needs
    the `-s`-with-direction form.
43. **`main-pane-width`/`main-pane-height` are baked into a ratio at
    `select-layout`/`next-layout` apply-time, not stored as an absolute
    size** (Task 6, layout presets). `Layout`'s tree only ever stores `f32`
    split ratios (no absolute-size node variant), so `apply_preset` computes
    `ratio_for(target_absolute_cells, area_len)` ONCE, at application time —
    the first render reproduces the exact configured main-pane cell count,
    but a LATER window resize scales the main pane proportionally rather
    than preserving the literal configured width/height the way real tmux
    does (tmux recomputes the absolute size on every resize). One-line fix
    framing: preserve absolute main-pane size across resize like tmux — would
    need a `Layout` node variant (or side-table) that remembers "this split's
    first child wants N absolute cells" and re-derives the ratio from the
    CURRENT area on every resize/render, not just at apply-time. Functionally
    acceptable for now (documented deviation, not a bug); doc gap closed by
    this same fix round (see `docs/specs/2026-07-07-parity-polish-interfaces.md`'s
    `layout-presets` section).

44. **`break-pane` has no `-s`/`-t` pane-selector flag** (Task 7, window
    ops). Real tmux's `break-pane` can target any pane via `-s`; winmux's
    always acts on the resolved CURRENT pane (matches the design spec's
    `## 6. Window ops` signature, which itself omits a pane selector —
    smaller, honest scope, same pattern as `swap-pane`'s own documented
    `-s`/`-t` deviations, follow-ups #41/#42). One-line fix framing: add an
    optional pane target parsed the same way `kill-pane -t`/`swap-pane -t`
    already are, threading it through `resolve_pane_target` instead of the
    current hardcoded `None`.

45. **`move-window` cannot move a window to a DIFFERENT session** (Task 7).
    Real tmux's `move-window -t <session:index>` can relocate a window
    across sessions; winmux's `move_window` (`model.rs`) is same-session
    re-indexing only, and `exec_move_window` explicitly discards any
    `session:` prefix on the `-t` value. Matches the design spec's `## 6.
    Window ops` framing ("re-index current window"). One-line fix framing:
    would need a cross-session variant of `Session::move_window` that lifts
    the `Window` out of one `Session.windows` and into another's, re-minting
    nothing (the `WindowId` stays valid — ids are global) but re-running the
    destination session's `lowest_unused_index` floor if no explicit index
    is given.

46. **`find-window` always jumps to the first match — no choose-list for
    multiple matches** (Task 7). The design spec's `## 6. Window ops`
    section is explicit about this ("jump to first match"), so this is NOT
    a shortfall against the spec of record, but it IS a real simplification
    relative to actual tmux (which shows a `choose-tree`-style picker when
    more than one window matches). Once Task 8's choose-tree overlay lands
    (design spec `## 7. Overlays`), `find-window` could route multi-match
    results through it instead of the deterministic first-match jump —
    tracked here so that follow-up wiring has a home.

## Deferred from sub-project 4 (parity polish, closeout — 2026-07-08)

Ticketed by Task 10 (e2e + docs closeout) from `docs/specs/2026-07-07-parity-polish-design.md`'s
"## Documented deferrals" list (its closing line: "ticket in follow-ups.md at
closeout"). Every item below was a DELIBERATE v1 scope decision documented in
the design spec at the time its owning task shipped, not a bug discovered
after the fact — cross-referenced against follow-ups #34-46 above to avoid
duplicates (mouse-forwarding-to-pane-apps is already #35; automatic-rename's
`allow-rename`/ESC k gap was noted inline at #28 but explicitly left
"not separately itemized" there, so it gets its own ticket here as promised).
None block the sub-project 4 merge.

47. **Scrollback does not reflow on terminal resize** (Task 1, grid v2). Real
    tmux (≥1.9) reflows scrollback content to the new width on resize;
    winmux's `VecDeque<Vec<Cell>>` scrollback lines are clipped/padded to the
    new width lazily on READ instead (design spec `## 1. Grid`: "NO reflow
    on resize ... documented winmux divergence, ticket"). A resize mid-copy-
    mode-scroll can therefore show ragged/truncated historical lines that
    don't match what a reflowing terminal would show.
48. **`choose-buffer` (`=`) is not implemented** (Task 3, paste buffers). The
    design spec explicitly deferred a picker UI for selecting among multiple
    named/automatic paste buffers; `paste-buffer`/`delete-buffer` always
    default to the newest buffer (or an explicit `-b name`) with no
    interactive chooser.
49. **`D` (choose-client) is not implemented** (Task 7, window ops; design
    spec `## 6. Window ops`: "`D` choose-client: DEFERRED"). There is
    no way to list and switch/detach OTHER attached clients from within a
    session; only `switch-client`'s session-level `(`/`)` and the CLI's
    `detach-client` exist.
50. **`choose-tree` has no preview, tagging, filtering, or sort options**
    (Task 8, overlays). The design spec's `## 7. Overlays` section is
    explicit ("No preview, no tagging (documented)"); winmux's `w`/`s`
    overlay is a flat, unfilterable, untaggable list with plain up/down
    navigation and no session/window content preview pane, unlike real
    tmux's `choose-tree`.

    **NARROWED (SP6 parity wave 2, Task 8, 2026-07-10).** The tree-shape and
    preview halves of this gap are now closed: `choose-tree` is a real
    session/window tree with `Left`/`Right` expand/collapse (sessions
    collapsed by default in `s`-view), the default selection lands on the
    CURRENT item (not always the first row), and `v` cycles a live preview
    box through off → BIG → normal with tmux's own sizing and full 4-sided
    box chrome. What remains open from the original text: **tagging** (no
    way to mark multiple rows for a bulk action) and **sort options** (real
    tmux's `O`/`r` cycle the sort key and reverse it; winmux has no sort
    concept at all, rows are always in registry-insertion order) — filtering
    (`/`-style incremental search within the list) is also still absent.
51. **No right-click context menus** (mouse, Task 5). Real tmux (recent
    versions) can show a right-click menu over a pane/status-line/border;
    winmux's mouse routing has no menu concept at all — every mouse event
    resolves to a direct action (focus/resize/select/scroll) or is dropped,
    never a menu.
52. **`allow-rename` (`ESC k` / the `#{automatic-rename}` toggle escape) is
    not implemented** (Task 9, automatic-rename; see also follow-up #28,
    which noted this gap inline but deferred the formal ticket to this
    closeout). `automatic-rename` is a global/window flag only
    (`set -g automatic-rename off` / `rename-window` disabling it
    permanently for that window); there is no per-pane-application escape
    sequence to toggle it transiently the way real tmux's `allow-rename`
    plus `ESC k` support.
53. **`paste-buffer -p`'s bracketed-paste flag is accepted but has no
    effect** (Task 3, paste buffers). Real tmux's `-p` wraps the pasted
    bytes in `ESC[200~`/`ESC[201~` bracketed-paste markers so a
    bracketed-paste-aware application (e.g. a shell with `bracketed-paste`
    support) can distinguish pasted text from typed keystrokes; winmux's
    `-p` is parsed and accepted but the write is always a plain byte dump
    (design spec `## 3. Paste buffers`: "`-p` accepted and ignored,
    documented").
54. **`find-window` matching is plain case-insensitive substring, not
    regex** (Task 7, window ops; design spec `## 6. Window ops`: "v1 no
    regex"). A pattern like `^foo` or `bar$` is matched LITERALLY (including
    the `^`/`$` characters) rather than as an anchor, unlike real tmux's
    `-r`-capable regex matching.
55. **No `copy-pipe`/OSC 52 clipboard integration in copy mode** (Tasks 2-4,
    copy mode). Copying in copy mode only ever writes to winmux's own
    internal paste-buffer store; there is no way to pipe a copy-mode
    selection to an external command (tmux's `copy-pipe`/`copy-pipe-and-cancel`)
    or to emit an OSC 52 sequence so the selection also lands on the REAL
    system clipboard (some tmux configs wire this up for clipboard
    integration over SSH).
56. **Emacs copy-mode table omits `C-k` (copy-to-end-of-line-and-cancel) and
    `M-m` (back-to-indentation)** (Task 2, copy mode; design spec `## 2. Copy
    mode`: "`C-k` copy to end of line and cancel (defer, not in v1 tables)").
    Both are real tmux emacs-table bindings; winmux's emacs copy-mode table
    covers the more commonly used subset only.
57. **Mouse bindings have no bindable NAMES** (`MouseDown1Pane`,
    `MouseDrag1Border`, etc.) **in `bind-key`** (Task 5, mouse; design spec
    `## 4. Mouse`: "Mouse \"bindings\" are HARDCODED v1 ... the bindings
    table stays keyboard-only"). Every mouse behavior (click-focus,
    border-drag-resize, wheel-scroll, etc.) is wired directly in
    `server::dispatch::dispatch_mouse`/`mouse_down`/`mouse_wheel` rather than
    going through the `Bindings` table, so a user cannot `bind-key -T
    root MouseDown1Pane <command>` to customize mouse behavior the way real
    tmux allows.

## Test-flakiness follow-up (Task 10 closeout verification, 2026-07-08)

58. **Concurrent-output copy-mode/selection tests in `tests/server_proto.rs`
    have flaked under full-parallelism `cargo test`.** During Task 10's own
    pre-merge verification, `selection_survives_concurrent_output` was
    reported to have flaked twice previously (passing standalone and at
    `--test-threads=4`), and in this task's own repeated `cargo test` runs
    `other_end_survives_concurrent_output` (same shape: asserts a
    content-pinned copy-mode endpoint stays correct while a background
    thread concurrently feeds the pane new output) flaked once out of three
    full-suite runs — also confirmed to pass standalone and at
    `--test-threads=4` immediately after. Both tests are inherently
    timing-sensitive (they race a real background writer thread against the
    main assertion under whatever scheduling a fully-parallel `cargo test`
    run gives the process), consistent with the project's existing
    documented `server_proto` flakiness class (CLAUDE.md's "Commands"
    section, `docs/follow-ups.md` general framing). Candidate fixes: widen
    the tests' timing margins, or run the affected tests (or all of
    `server_proto`) at a capped thread count in CI rather than full
    default parallelism.

## Follow-ups from the final whole-branch review (sub-project 4, 2026-07-08)

59. **`'` index-prompt empty-commit is a silent no-op; `MoveWindow`/
    `RenameWindow`/`RenameSession` siblings all error on empty instead**
    (Task 7/8, prompt commit handling). `PromptKind::Index`'s empty-buffer
    commit is a deliberate, comment-documented silent no-op
    (`src/server/dispatch.rs`, matching `PromptKind::Command`'s own
    empty-commit-is-silent-cancel precedent), whereas `RenameWindow`/
    `RenameSession` error via `model::validate_name` rejecting an empty
    name, and `MoveWindow` errors via a failed `u32` parse on an empty
    target. Verified by the final SP4 review as a real inconsistency
    between sibling prompts, but a reasoned judgment call (empty `'` input
    genuinely means "stay on the current window," unlike an empty rename/
    move target, which has no sensible default) rather than a defect.
    Ticketed so the deliberateness is durable and revisitable if the
    inconsistency ever surprises a user.
60. **`swap-pane -t` first/last wraparound has no dedicated test**
    (Task 6, layout presets). The wrap arithmetic
    (`src/server/dispatch.rs`, `(pos+n-1)%n` / `(pos+1)%n`) is standard and
    the same pattern is already pretested via `rotate`, but no test
    exercises `pos == 0` swapping `Up` (should wrap to the last pane) or
    `pos == n-1` swapping `Down` (should wrap to pane 0) for `n >= 3` panes.
    Low-risk coverage gap, not a known bug -- add a
    `swap_pane_wraps_at_ends` (or similar) `server_proto` test.
61. **Choose-tree ignores mouse entirely -- no click-to-select or
    wheel-to-scroll routing** (Task 8, overlays; amends #38's scope). The
    final SP4 review's merge-gate fix made `dispatch_mouse` swallow every
    mouse event while `ChooseTree`/`DisplayPanes` is open (fixing the
    hidden-pane-focus leak, see #38), but real tmux lets the mouse interact
    WITH an open choose-tree: a click on a row selects it, wheel scrolls
    the list. winmux v1 has no such routing at all -- a deliberate scope
    cut for this fix round, not a regression (there was never any
    choose-tree mouse routing to begin with; the bug fixed here was mouse
    leaking THROUGH the overlay to the hidden panes underneath, not a
    missing choose-tree mouse feature). Candidate follow-on: hit-test the
    overlay's own row rects (mirroring `mouse_status_row`'s pattern) and
    map a click to `ChooseTreeAction::Commit`-equivalent behavior, wheel to
    `Up`/`Down`.
62. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10). Doc comments
    added on both `Window::name` and `Session::name` (`src/model.rs`)
    pinning the invariant ("only ever set via a `validate_name`-gated
    setter") and the render-time trust it protects, so the risk is
    documented at the field, not only at today's call sites. No code
    behavior change (doc-only ticket, no RED test needed per the task
    brief).
    *Original text:* `Window::name`/`Session::name` field safety relies on
    an unstated
    invariant (Task 9, naming; security audit finding). Both fields are
    plain `String`s with no type-level guarantee that every write went
    through `model::validate_name` -- the invariant holds today only
    because every call site that sets a name (`exec_rename_window`,
    `exec_rename_session`, `derive_auto_name`'s caller) happens to be
    gated by it, and choose-tree row rendering (`src/server/dispatch.rs`,
    `TreeRow` text building) plus the status bar trust that transitively
    when interpolating names into rendered VT output with no further
    escaping. No exploit exists today (verified in the final SP4 review's
    security pass), but the invariant is easy to silently break in a
    future refactor that adds a new direct-assignment call site. Add a doc
    comment on `Window::name`/`Session::name` pinning "only ever set via a
    `validate_name`-gated setter" so the risk is documented at the field,
    not just at today's call sites.
63. **RESOLVED** (SP7 Task 4, plumbing debt batch, 2026-07-10).
    `render::CopyView::cursor` is deleted; the one construction site
    (`src/server.rs`'s `render_one`) drops `cursor: (cs.cx, cs.cy)` from its
    `CopyView { .. }` literal in the same commit. Contract amendments:
    `2026-07-06-mvp-interfaces.md`'s `## render` section (new amendment note)
    and `2026-07-07-parity-polish-interfaces.md`'s `## render` amendment
    (under `## copy-mode`).
    *Original text:* `render::CopyView::cursor` is a dead field (Task 2/3,
    copy mode;
    security audit finding). `server::render_one` populates
    `CopyView { scroll, cursor: (cs.cx, cs.cy), sel }` for the pane bound
    to a client's `ClientMode::Copy`, but `render.rs`'s
    `Renderer::compose_back` never reads `cv.cursor` -- only `cv.scroll`
    (view-cell lookups, position indicator) and `cv.sel` (selection
    highlight) are consumed. The actual terminal cursor placement during
    copy mode is computed independently in `server::render_one`'s own
    `(cursor, cursor_visible)` match on `client.mode` (clamping
    `cs.cx`/`cs.cy` into the pane rect directly), not through `CopyView` at
    all. `CopyView` is part of the locked render interface contract
    (`docs/specs/2026-07-06-mvp-interfaces.md` and the `## copy-mode`
    section of `docs/specs/2026-07-07-parity-polish-interfaces.md`), so
    removing the field requires a contract amendment in both files plus
    updating the one construction site (`src/server.rs`) -- deferred here
    rather than folded into this fix commit to keep that commit's
    contract-surgery scope to the items it was already touching; genuinely
    safe to delete whenever someone picks this up (no consumer anywhere
    reads it).

64. **RESOLVED** (SP6 parity wave 2, Task 1: mouse drag-state lifecycle
    fix). *Original text:* Stale `MouseDrag` state when an overlay opens
    mid-drag (found in the final SP4 fix-wave re-review). The overlay mouse
    guard in `dispatch_mouse` (`src/server/dispatch.rs`) swallows mouse
    events while choose-tree/display-panes are open but does not clear
    `client.mouse.drag`, unlike the sibling "outside pane area" guard which
    explicitly resets it. A drag armed before an overlay opens (keyboard-
    triggered overlay mid-drag, or a `display-panes -d` timer expiry) can
    leave stale `Border`/`Selecting` state alive across the overlay's
    lifetime, revivable by a later out-of-sequence `Drag`/`Up` frame with no
    intervening `Down`. Not reachable from a conformant terminal's mouse
    protocol (real terminals always send Down before Drag/Up), hence LOW and
    non-blocking; fix is a one-liner (`client.mouse.drag = MouseDrag::None`
    in the overlay guard arm) plus a test that arms a drag, opens an
    overlay, and asserts the drag state is cleared.

    **Fixed:** a wider SP6 gap-analysis pass (`.superpowers/sdd/sp6-gap-analysis.md`
    §D) found this exact defect class on two OTHER `dispatch_mouse`
    early-return paths beyond the overlay guard this entry originally
    ticketed — the status-row short-circuit (`ev.y == status row` diverts
    Drag/Up to `dispatch_mouse_status`, which ignores them) and
    `mouse_down`'s `MouseHit::None` arm (a press that misses every
    pane/border cell) — and diagnosed the resulting user-visible symptom
    ("border drag works once then dies"). All three now reset
    `client.mouse.drag = MouseDrag::None` before their early return, mirroring
    the "outside pane area" guard's existing pattern:
    `src/server/dispatch.rs`'s `dispatch_mouse` (overlay guard and
    status-row short-circuit) and `mouse_down`'s `MouseHit::None` arm.
    Regression coverage: `tests/server_proto.rs`'s
    `mouse_border_drag_twice_resizes_twice` (non-regression baseline for two
    clean consecutive drags) and
    `mouse_border_drag_release_on_status_row_then_drag_again` (genuine
    RED/GREEN reproduction: an out-of-sequence `Drag` with no `Down`, sent
    right after a status-row-swallowed release, must not spuriously move the
    border), plus `src/server/dispatch.rs`'s `mouse_dispatch_tests` module
    (`mouse_drag_cleared_when_overlay_swallows_release`,
    `mouse_down_miss_clears_stale_drag`) for the overlay and
    `MouseHit::None` legs, which — like this entry's original note says —
    aren't reachable through a conformant SGR stream and are exercised
    directly against `Server`/`ClientState` instead.

65. **RESOLVED** (SP6 parity wave 2, Task 3: edge-wrap directional
    navigation + real `active_point` MRU). *Original text:* Directional
    focus MRU tie-break is a single-slot approximation (from the focus-nav
    hotfix `6e6ff4d`, its review). When multiple panes are valid candidates
    for `select-pane -L/-R/-U/-D`, real tmux picks the most recently used;
    winmux uses the window's single last-pane slot if it is among the
    candidates, else the first candidate in pane-index order. Correct for
    the 2-candidate case and deterministic otherwise; a full per-window MRU
    ordering would make 3+-candidate columns match tmux exactly. Also noted
    by the review: no test drives the pre-fix bug via Left/Up specifically
    (fix is symmetric per axis pair), and none exercises focus_dir while
    zoomed — coverage niceties.

    **Fixed:** two changes, both in `src/layout.rs::Layout::focus_dir`
    (contract-amended in `docs/specs/2026-07-06-mvp-interfaces.md`). (a)
    **Edge-flip wrap** (`docs/tmux-reference/panes-and-layout.md` §1.1,
    `window_pane_find_left/right/up/down`): the four adjacency arms now
    compute a search edge that flips to one past the FAR side of `area`
    when the focused pane is already flush against the near side, so
    directional navigation wraps (Left from the leftmost pane reaches the
    rightmost, symmetric in all four directions) instead of silently
    no-op'ing at a window edge. (b) **Real per-pane `active_point` MRU**:
    the single-slot `last_focused` approximation is replaced by a real
    tmux-style recency counter — `focus_dir` gained an `activity: &dyn
    Fn(PaneId) -> u64` parameter, and the tie-break is now the candidate
    with the greatest `activity(pane)` (ties fall back to first-in-leaf-
    order, matching tmux's strict-`>` `window_pane_choose_best` loop
    exactly), correctly ranking 3+-candidate columns. The counter itself
    (`Server::pane_activity: HashMap<PaneId, u64>` +
    `Server::next_active_point`, `src/server.rs`) is server-global (like
    tmux's, meaningful across windows/sessions) and stamped by
    `Server::stamp_active` at every `window_set_active_pane`-equivalent
    call site in `src/server/dispatch.rs`: `exec_select_pane`'s
    `focus_dir` and `focus_pane` branches, `exec_split_window`/
    `exec_new_window`/`exec_new_session` (a newly created pane is
    focused, hence most recent), `exec_last_pane`, `mouse_focus_pane`
    (mouse click focus, also reused by the `display-panes` digit-jump),
    and `exec_rotate_window` (tmux `cmd-rotate-window.c:109` calls
    `window_set_active_pane`). Death handoffs (`kill_pane_by_id`,
    `exec_break_pane`'s source-window reassignment, `handle_exited`'s
    natural-exit reassignment) deliberately do NOT stamp: tmux's
    `window_lost_pane` (window.c) reassigns `w->active` directly
    (last_panes stack -> prev -> next) with no `active_point` bump, so
    the surviving pane keeps its historical recency (SP6 Task 3 fix
    round 3, verified directly against the tmux C source). Neither does
    `exec_break_pane`'s moved pane (fix round 4): the classic break-pane
    path (cmd-break-pane.c:153-158) sets `w->active = wp` by direct
    assignment too -- tmux only stamps freshly SPAWNED panes (spawn.c),
    not break-pane's recycled one -- so `exec_break_pane` stamps nothing
    on either side. `pane_activity` entries are pruned wherever panes
    are dropped (mirrors `last_rects` cleanup exactly).
    `Layout::last_focused`
    itself is RETAINED, but narrowed to its one remaining job,
    `focus_last`/the `prefix ;` toggle — an unrelated tmux feature, not
    part of `focus_dir`'s algorithm anymore. `Layout::focus_next` has no
    production call site today (dead outside unit tests, `o` is bound to
    `select-pane -t :.+` instead), so it was left unstamped; a future
    binding of it would need the same `Server::stamp_active` call the other
    five sites get. New tests: `src/layout.rs`'s
    `focus_dir_wraps_left_to_rightmost`, `focus_dir_wraps_down_to_top`,
    `focus_dir_wrap_picks_most_recently_active_of_two_far_candidates`,
    `focus_dir_three_candidates_ranked_by_activity` (this entry's exact
    3-candidate gap), and
    `focus_dir_ties_fall_back_to_first_candidate_in_leaf_order` (replaces
    the old last_focused-fallback test, whose premise no longer exists
    once every pane always has a real activity value); the pre-existing
    `focus_dir_two_pane_horizontal`'s at-edge `false` assertions were
    inverted to the new wrap outcome (computed comments explain why).
    `tests/server_proto.rs`'s `focus_wraps_at_window_edge` covers the same
    wrap end-to-end through a real client/server pair. Zoom interaction
    unchanged/preserved: `focus_dir` still reads `all_rects` (the unzoomed
    full-tree geometry) regardless of `self.zoomed`, so directional nav
    while zoomed moves which pane is zoomed-in, exactly as before this fix
    — not itself a regression target of this task, still an
    untested-but-unchanged edge per the original note.

66. **RESOLVED** (SP6 parity wave 2, Task 1b). *Original text:* Mouse
    border-drag toward the top/left edge never resizes (found while
    building Task 1's regression tests, SP6 parity wave 2; separate root
    cause from #64's staleness class, out of that task's scope). A vertical
    border drag's reference pane, from `mouse_down`'s `MouseHit::VBorder {
    left }` (and `HBorder { top }` for horizontal borders), is bound ONCE at
    `Down` time and reused unchanged for the whole gesture in
    `mouse_drag_border` (`src/server/dispatch.rs`). But
    `Layout::resize_from` (`src/layout.rs`) only accepts that reference for
    `Direction::Right`/`Direction::Down` (which grow the split's FIRST
    child, matching `left`/`top`'s first-child position) — `Direction::Left`/
    `Direction::Up` (shrink the first child / grow the second) require the
    SECOND-child pane as reference instead (see
    `layout::tests::resize_from_reference_pane_ignores_focus`), which
    `mouse_drag_border` never resolves. Net effect: dragging a border toward
    the pane's own left/top edge (shrinking the LEFT/TOP pane, growing the
    RIGHT/BOTTOM one) is a silent no-op, unconditionally and reproducibly —
    confirmed empirically with a single, otherwise-correct drag (no staleness
    involved at all). Plausibly the dominant real-world contributor to the
    "border drag works once then dies" user reports #64/this class of fix
    addressed, since a user alternating drag directions would see exactly
    that symptom. Fix sketch: `mouse_drag_border` (or `mouse_down`'s
    `VBorder`/`HBorder` arms) needs to resolve the CORRECT reference pane
    per-direction each call (or store both siblings at `Down` time and pick
    based on `delta`'s sign), then re-verify against
    `layout::tests::resize_from_reference_pane_ignores_focus`'s contract.
    Not fixed here — orthogonal to Task 1's drag-STATE-lifecycle scope.

    **RESOLVED** (SP6 parity wave 2, Task 1b): fixed in `mouse_drag_border`
    (`src/server/dispatch.rs`) — rather than resizing against the `pane`
    bound once at `Down` (always the first-child/`left`/`top` side), it now
    resolves the correct `Layout::resize_from` reference fresh on every
    `Drag` call, per the resolved direction: unchanged (`pane`) for
    `Direction::Right`/`Down`, but for `Direction::Left`/`Up` it looks up
    `pane`'s current neighbor across that exact border cell (the pane
    starting one cell past the border, since a border occupies its own
    column/row between panes) and uses that as the reference instead.
    `Layout::resize_from`'s public contract and signature are unchanged (no
    contract-doc update needed) — a pure `dispatch.rs`-side fix. Tests:
    `layout::tests::resize_from_first_child_reference_rejects_shrink_direction`
    (`src/layout.rs`, pins the pre-existing `resize_from` contract the bug
    traces to) plus two new `tests/server_proto.rs` regression tests,
    `mouse_border_drag_resizes_leftward` and
    `mouse_border_drag_resizes_upward`, which reproduced the bug RED (timed
    out waiting for the border to move) before the fix and pass GREEN after.
    Full `cargo test` and `cargo clippy --all-targets -- -D warnings` clean.

67. **`unbind` on an unparseable key is unconditionally silent (not `-q`-gated), and
    mouse pseudo-keys are not table-driven** (found in SP6 Task 2 review, 2026-07-10).
    Two related fidelity gaps behind one shim: (a) `exec_unbind_key`
    (`src/server/dispatch.rs`) silently no-ops for ANY token `keys::parse_key`
    rejects — real tmux errors on `unknown key: %s` unless `-q` is given
    (docs/tmux-reference/commands-config-options-formats.md:442), so a typo like
    `unbind Ct-x` is swallowed today. Fix sketch: silence only tokens matching the
    tmux mouse-pseudo-key name grammar (`MouseDown/Up/Drag/DragEnd{1,2,3}`,
    `WheelUp/Down`, `Double/TripleClick{1,2,3}` × `Pane/Border/Status/StatusLeft/
    StatusRight/StatusDefault`), error otherwise unless `-q`. (b) Deeper: winmux's
    mouse actions are hardcoded in dispatch rather than resolved through the key
    tables, so `unbind -T copy-mode-vi MouseDragEnd1Pane` (the user's real config —
    tmux idiom for "don't copy/jump on mouse release in vi copy mode") parses clean
    but cannot have its tmux effect. Real fix is table-driven mouse bindings
    (synthesized mouse key names resolved through `Bindings` like tmux); sizeable,
    interacts with SP6 Tasks 6-7's release-semantics work. LOW/MEDIUM.

68. **`show-options -v`/`-q` CLI flags not wired into dispatch** (found in SP6
    Task 2, 2026-07-10). `Options::show_user_option` implements tmux-correct
    value-only/quiet semantics at the options layer (unit-tested), but `cmd.rs`
    doesn't parse `-v`/`-q` for `show-options`/`show`, so `show -gqv "@foo"` — the
    exact TPM rung-1 primitive (docs/superpowers/plans/2026-07-08-tpm-plugin-support.md)
    — is not yet available end-to-end. Fix: parse the flags in cmd.rs, thread to
    `exec_show_options`. SMALL. LOW (until SP5/TPM work starts).

69. **`status-justify` has no overflow/scroll behavior, and `status-left`'s
    length cap counts `#[...]` marker bytes as visible width** (SP6 Task 4,
    2026-07-10, `status::list_offset`/`status_spans`). When `left` + the
    window list + `right` together exceed the terminal width under
    `centre`/`right`/`absolute-centre` justify, real tmux scrolls the window
    list around the focused window and draws `<`/`>` overflow markers
    (`docs/tmux-reference/status-line-and-messages.md` §1.4); winmux instead
    saturates the computed padding to zero (list abuts `left` with no gap,
    no markers, no scrolling) — the row can visually overlap/overrun in a
    narrow terminal instead of scrolling. Separately, `status-left`'s
    `status-left-length` cap (`server.rs`'s `truncate_chars`) is applied to
    the RAW `expand_format` output, which may still contain verbatim
    `#[...]` inline style markers (SP6 Task 4's `#[...]` passthrough) — a
    marker's characters count toward the length budget even though they
    draw zero visible columns, and a cap could theoretically bisect a
    marker. `status-right` doesn't have this problem (`strip_style_markers`
    runs before its length cap). Not exercised by any current config
    (`status-left` is empty in the fixture, and no config combines
    length-capping with inline `status-left` markers) — same "not
    implementing tmux's degenerate-width scrolling" bucket as follow-up #46
    (`find-window`) and #50 (`choose-tree` overflow). SMALL/MEDIUM. LOW.

70. **Custom `window-status-format` values containing `#{?cond,a,b}` expand
    the conditional to empty** (SP6 Task 4 + fix round 1, 2026-07-10). What
    remains of the Task 4 default-format deviation after the fix-round-1
    width-stability shim: winmux's stored DEFAULT is
    `options::DEFAULT_WINDOW_STATUS_FORMAT` (`#I:#W#F`) plus a
    default-path-only pad of an empty flags string to one space in
    `status::status_spans` — together byte-identical to tmux's real default
    `#I:#W#{?window_flags,#{window_flags}, }` for flagged AND flagless
    windows, width-stable across focus changes. But a USER-set format that
    itself uses `#{?...}` (or any other conditional/modifier outside the
    `expand_format` subset) still renders that token as empty — the general
    tmux format-expression engine is deliberately deferred to the TPM plan
    (`docs/superpowers/plans/2026-07-08-tpm-plugin-support.md`). The three
    doc sites describing the deviation: `src/options.rs`
    (`DEFAULT_WINDOW_STATUS_FORMAT` + the `default_value` arm's note),
    `docs/specs/2026-07-07-command-config-interfaces.md` (options SPECS
    amendment), `docs/specs/2026-07-07-server-client-interfaces.md`
    (`## status` flags-string/padding-shim rule). Not exercised by the
    fixture config (its custom formats use only `#I`/`#W`/`#F`/`#[...]`).
    MEDIUM (a format engine). LOW.

71. **Per-window `FormatCtx` reuses the FOCUSED pane's `pane_index`/
    `pane_title` for every window's tab expansion** (SP6 Task 4 review
    Minor, 2026-07-10, `status::status_spans`). Each tab's
    `window-status(-current)-format` expansion overrides
    `window_index`/`window_name`/`window_flags` per window but carries the
    caller's `pane_index`/`pane_title` (the acting client's focused pane in
    the CURRENT window) unchanged — so `#P`/`#T` inside a per-window format
    misrender for every non-focused window (they show the focused window's
    values). Root cause: only one pane's title/index is threaded through
    `server::render_one`'s status pipeline; fixing it needs per-window
    active-pane data in `status::WindowEntry` (or the ctx). Not exercised
    by the fixture config (`#I`/`#W`/`#F` only). SMALL. LOW.

72. **No application mouse passthrough** (adjudicated in SP6 Task 6 review, 2026-07-10).
    winmux never relays raw mouse bytes to pane applications: a pane program that
    enables mouse reporting (vim, htop, less --mouse) receives nothing; wheel input
    is translated to 3x arrow keys only (src/server/dispatch.rs wheel path). Real
    tmux forwards encoded mouse events to panes whose program requested mouse mode
    (docs/tmux-reference/mouse.md, passthrough rules: MOUSE_* pane flags gate
    consume-vs-forward, input_key_get_mouse re-encodes per the pane's requested
    protocol). Consequence: SP6's drag-on-live-pane-enters-copy-mode is
    unconditional where tmux would defer to a mouse-owning app. Real fix: track
    DECSET 1000/1002/1003/1006 from pane output in grid, re-encode and forward in
    dispatch, and gate copy-mode-entry/wheel translation on the pane's mouse mode.
    MEDIUM effort. Interacts with #67(b) table-driven mouse bindings.

73. **choose-tree degenerate tiny-pane guard reverts to a full-height list where
    tmux would draw a short list + blank remainder** (SP6 wave 2 Task 8 review,
    self-found, 2026-07-10, `dispatch::Server::choose_tree_list_height`). winmux
    folds tmux's mode_tree_draw paint-time guard (`sy <= 4 || h < 2 ||
    sy - h <= 4 || w <= 4`, mode-tree.c:980-981 — "don't draw the box") into the
    HEIGHT function by setting `h = sy` (list takes the whole panel). Real tmux
    keeps the computed `h` and simply skips painting the preview box, leaving rows
    `h..sy-1` blank — so in a degenerate-size pane (e.g. BIG preview mode in a
    panel 5-6 rows tall) tmux shows a short list over blank rows where winmux
    shows a full-height list. Defensible (winmux's behavior is arguably more
    useful — no dead rows) and reachable only in degenerate geometries; ticketed
    for the record. TINY. LOW.

## Follow-ups from Task 9 (SP6 parity wave 2 closeout, 2026-07-10)

74. **Alerts subsystem (`visual-activity`/`visual-bell`/`visual-silence`/
    `bell-action`/`monitor-activity`) is accepted/stored but has zero runtime
    effect.** `src/options.rs` has typed getters for all five
    (`visual_activity`/`visual_bell`/`visual_silence`/`bell_action`/
    `monitor_activity`), round-tripping through `set`/`show` with tmux's real
    defaults, but no other module in the codebase calls any of them (`grep`
    confirms every reference outside `options.rs` itself is in that same
    file's own unit tests) — there is no bell/activity DETECTION at all (no
    code watches for a BEL byte, an inactive window's pane producing new
    output, or a window going silent for `silence-monitor` seconds), let
    alone the window-flag (`#F` `~`/`#`/`!`) or visual-flash/message
    reaction real tmux would produce. The user's real `.tmux.conf` fixture
    (`tests/fixtures/user.tmux.conf`) sets all five to their
    least-surprising values (`off`/`off`/`off`/`none`/`off`) precisely
    because they don't want alerts, so this gap is invisible to that
    config's own test coverage — SP6's config-compat batch made every one of
    these options PARSE cleanly (closing the "unknown option" class of
    error), but implementing the alert-detection/reaction behavior itself
    was out of that batch's scope. Fix sketch: bell detection needs a BEL
    (`\x07`) hook in `grid`'s VT parser surfaced as a pane event; activity
    detection needs an "this pane produced output and its window isn't
    focused" check in the server's output-handling path; both would then
    feed a window-flags bit (already partially modeled via `#F`'s existing
    `*`/`-` flags, `src/options.rs`'s `expand_format`) plus, for the visual-*
    variants, a status-line flash/message. MEDIUM effort (new detection
    machinery, not just wiring an existing signal to an existing option).
    LOW priority (no known user config actually turns these on and depends
    on the behavior; the one polled fixture explicitly turns them off).
