# Contributing to AudioNet

Thank you for helping. AudioNet is a real-time audio system and an
accessibility-first product, so a few rules matter more here than in most
projects.

## Before you start

* Read [AGENTS.md](AGENTS.md). It is the project's authoritative guide to
  real-time audio engineering: no blocking or allocation in audio
  callbacks, bounded queues, clock drift handled by continuous resampling,
  and **measure before tuning**.
* Read [docs/architecture.md](docs/architecture.md) for how the pieces fit.
* For anything larger than a small fix, open an issue first to discuss the
  approach.
* Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).
* Security problems go to private vulnerability reporting, not public
  issues (see [SECURITY.md](SECURITY.md)).

## Development

```text
cargo build
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

All four must pass; GitHub Actions runs them on Linux, Windows and macOS
for every pull request. The simulation suite (`cargo test -p audionet-engine
--test sim`) takes a few minutes; run it for any change to the receiver,
playout or controllers. See [docs/testing.md](docs/testing.md) for hardware
and soak testing.

## Rules for audio changes

For any change to the real-time path, include in the pull request:

```text
Hypothesis:
Evidence:
Change:
Expected metric change:
Risks:
Verification:
```

and answer the review checklist in AGENTS.md §44 (does it allocate, block,
lock, do I/O, grow a queue, change the playback clock...?). Do not change
buffer sizes, frame sizes, priorities or controller gains without
measurements showing why.

Report verification honestly and separately: compiled, unit-tested,
simulated, tested on hardware, tested with a screen reader, soak-tested.

## Rules for user interfaces

* Every control is a native, labelled control reachable by keyboard.
* Every piece of important state is available as text. No meaning is
  conveyed by color, graphs, meters or position alone.
* Announce state changes once; never spam screen readers.
* Do not claim NVDA, JAWS, VoiceOver or TalkBack support without testing
  with that screen reader. Describe what you tested.

## Code conventions

* Rust 2024 edition, `rustfmt` defaults.
* `unsafe` is allowed only in platform backend crates, and every block
  needs a `// SAFETY:` comment.
* JSON: `snake_case`, every field always present (`null` rather than
  omitted), documented in [docs/protocol.md](docs/protocol.md).
* Keep deployment-specific values (host names, secrets) out of code.
  Official-instance settings live only in `deploy/official/`.

## Licensing

By contributing, you agree that your contributions are licensed under the
MIT license, as the rest of the project.
