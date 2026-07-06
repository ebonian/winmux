# Follow-ups from the MVP final review (2026-07-07)

Non-blocking items ticketed by the final whole-branch review of the
multiplexing MVP (branch `feature/multiplexing-mvp`). None affect the merge.

1. **Dead panes retain their `Pty` until closed.** `app.rs` `Event::Exited`
   only sets `dead = true`; the contract says the Pty should be dropped there
   (freeing the pseudoconsole/conhost and unblocking the reader thread).
   Bounded, harmless; reconcile code or contract.
2. **Confirm race when `Ctrl-b x y` arrive in one stdin read.** The `y` is
   forwarded to the shell because confirm mode arms only after the batch is
   processed. Rare interactively; benign. Would need feed-time arming or a
   two-pass protocol.
3. **`Host::enter()` partial-failure gap.** Code pages/stdout mode are mutated
   before the `RESTORE` snapshot is published; a failure in between (near
   impossible) would leave them unrestored. Publish `RESTORE` before the first
   mutation.
4. **Unbounded event-channel growth under pane output flood.** One render per
   4 KB `Output` chunk; coalesce drained events before rendering to bound the
   queue.
5. **`layout` Right/Down adjacency `u16` overflow (theoretical).** Harden
   `f.x + f.w + 1` with `saturating_add` for consistency.
6. **`grid::cell()` panic message lacks coordinates.** Trivial debuggability
   improvement.
