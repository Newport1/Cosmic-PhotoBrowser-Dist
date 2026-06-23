# PhotoBrowser Release Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Release PhotoBrowser with one consistent application identity and without local fixture paths or internal release-process language.

**Architecture:** The rename is namespace-wide: Cargo identity, runtime state, persistent locations, environment interfaces, and public documentation move together. Cleanup-only edits do not alter photo browsing behavior; fixture tests become explicitly opt-in and unsafe optional access paths are removed.

**Tech Stack:** Rust 2021, Cargo, libcosmic, serde, SQLite, XMP.

**Status:** Completed on 2026-06-23. The macOS dependency configuration keeps the Linux Wayland feature set intact while using the native winit/wgpu configuration locally.

---

### Task 1: Establish the new Cargo and application identity

**Files:**
- Modify: `Cargo.toml`, `Cargo.lock`, `src/lib.rs`, `src/main.rs`, `src/app/mod.rs`
- Test: Cargo package metadata and compilation

- [ ] Change the package, library, and binary names to `photobrowser`; update the binary entry point to call `photobrowser::run()`.
- [ ] Update all application-visible title, startup log, and application-ID values to `PhotoBrowser` / `com.photobrowser.PhotoBrowser`.
- [ ] Add the public package metadata:

```toml
readme = "README.md"
repository = "https://github.com/Newport1/COSMIC_PhotoBrowser"
keywords = ["photo", "cosmic", "raw", "browser", "xmp"]
categories = ["multimedia::images", "filesystem"]
publish = false
```

- [ ] Run `cargo metadata --no-deps --format-version 1` and confirm the package and binary identity are `photobrowser`.

### Task 2: Migrate persistent locations and external configuration names

**Files:**
- Modify: `src/config.rs`, `src/catalog.rs`, `src/thumb/xdg.rs`, `src/thumb/worker.rs`, `src/decode.rs`, `src/metadata.rs`, `src/app/mod.rs`
- Test: existing config, thumbnail-cache, fixture, and duplicate-scan tests

- [ ] Change project-directory components, default export directory, cache version key, worker thread name, and all test fixture paths to the new identity.
- [ ] Rename every app-owned environment variable prefix to `PHOTOBROWSER_`, including thumbnail cache, fixture, benchmark, and duplicate fixture variables.
- [ ] Replace each ignored fixture test's local-path fallback with explicit environment lookup and a non-sensitive skip message. Use this pattern:

```rust
let Ok(candidate) = std::env::var("PHOTOBROWSER_TEST_NEF") else {
    eprintln!("SKIP: set PHOTOBROWSER_TEST_NEF to a local NEF fixture path");
    return;
};
```

- [ ] Run the focused test modules:

```bash
cargo test config::tests thumb::xdg::tests thumb::worker::tests decode::tests metadata::tests
```

### Task 3: Apply release-safety fixes

**Files:**
- Modify: `src/app/update_keyboard.rs`, `src/app/update_loupe.rs`, `src/app/update_duplicates.rs`
- Test: `src/app` unit tests and full suite

- [ ] Compute histograms from a local `handle` before storing it in the optional loupe field, removing the two optional-value unwraps.
- [ ] Replace slideshow and compare-state unwraps with `let Some(...) else { return Task::none(); };` guards.
- [ ] Split duplicate-mode assignments from background-task comments; retain the existing task scheduling and catalog-connection behavior.
- [ ] Run `cargo test` and confirm all tests pass.

### Task 4: Make source documentation release-facing

**Files:**
- Modify: `src/lib.rs`, `src/xmp.rs`, `src/preview_cache.rs`, `src/inspection.rs`, `src/cosmic_adapter.rs`, `src/decoded_image.rs`, `src/export.rs`, `src/thumb/mod.rs`, `src/thumb/cache.rs`, `src/app/update_decode.rs`, `src/view/mod.rs`, `src/view/grid.rs`, `src/decode.rs`, `src/dedupe.rs`, `src/app/mod.rs`, `src/browser_state.rs`, `src/view/loupe.rs`
- Test: Clippy and documentation build through compilation

- [ ] Replace phase, milestone, ownership, contract, and informal process comments with concise descriptions of current behavior.
- [ ] Keep existing public API signatures and test logic unchanged except where the namespace migration requires expected values to change.
- [ ] Run:

```bash
cargo clippy --all-targets -- -D warnings
```

### Task 5: Publish standard documentation and remove obsolete artifacts

**Files:**
- Create: `README.md`, `CHANGELOG.md`, `docs/ARCHITECTURE.md`
- Remove: the former generated public documents and release-review checklist
- Modify: `docs/superpowers/specs/2026-06-23-photobrowser-release-cleanup-design.md`

- [ ] Move the existing clean public documents to the standard public filenames and replace the previous app identity and CLI command with the new identity.
- [ ] Retain only user-facing capabilities, build/run instructions, write behavior, release history, and architecture; remove release-review artifacts from the shipped tree.
- [ ] Ensure the approved design note remains accurate after implementation.

### Task 6: Format, build, and prove the former identity is absent

**Files:**
- Modify: files touched by `cargo fmt`
- Test: whole repository

- [ ] Run:

```bash
cargo fmt --check
cargo test
cargo build --no-default-features
cargo clippy --all-targets -- -D warnings
rg -n -i 't[r]estle' -g '!target/**' .
```

- [ ] If formatting reports changes, run `cargo fmt`, then rerun all validation commands.
- [ ] Treat an empty final scan as mandatory. No Git commit is possible because this source folder has no Git repository metadata.
