# Contributing to Meridian OS

Thanks for your interest! Meridian is an original operating system; please
read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
[docs/KERNEL_DESIGN.md](docs/KERNEL_DESIGN.md) first.

## Ground rules

* **Original work only.** Do not submit code copied from Windows (leaked or
  decompiled), Linux, ReactOS or any source whose licence is incompatible
  with Apache-2.0. Reference public documentation instead. See
  [docs/LICENSES.md](docs/LICENSES.md) for the third-party code policy.
* **No fake features.** A control, API or setting must do what it says, or
  fail clearly. No placeholder success values.
* **Test it.** New kernel functionality needs a self test in
  `kernel/src/selftest.rs` (or the relevant test layer). Report results
  honestly.
* **No ads, tracking or telemetry**, ever.

## Workflow

1. Fork and create a branch.
2. `cargo fmt --all` and `python tools/mbuild.py test` must pass.
3. Update the docs you affected, including [docs/STATUS.md](docs/STATUS.md)
   and [docs/ROADMAP.md](docs/ROADMAP.md).
4. Open a pull request describing what changed and how you tested it.

## Code style

* Rust 2024 edition, `rustfmt.toml` settings, no third-party crates in the
  kernel without discussion.
* Every `unsafe` block gets a `// SAFETY:` comment explaining why it is sound.
* Module-level doc comments explain what a component does and its invariants.
