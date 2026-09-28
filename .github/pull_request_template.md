## What this changes

## How it was verified

Report each kind separately, and only what actually happened:

- [ ] Compiles; `cargo fmt`, `cargo clippy -- -D warnings` and `cargo test` pass
- [ ] Simulated (`cargo test -p audionet-engine --test sim`) — for receiver, playout or controller changes
- [ ] Tested on real hardware or a real network (say which)
- [ ] Tested with a screen reader (say which) — for user-interface changes
- [ ] Soak-tested (say how long)

## For changes to the real-time audio path

Hypothesis, Evidence, Change, Expected metric change, Risks, Verification
(see CONTRIBUTING.md), and the answers to the review checklist in
AGENTS.md §44.
