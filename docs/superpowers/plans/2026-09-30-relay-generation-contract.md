# Relay Generation Contract Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan.

**Goal:** Deliver independently testable Rust generation compilation and publication contracts, validated against real sing-box 1.14.2 traffic.

**Architecture:** Stable front selectors delegate to two leased policy banks. A strict pure ledger records generation identity, publication intent, actual observations and conservative reclamation proof. Existing production lifecycle remains on the current renderer until its ownership and dial barrier can be integrated safely.

**Tech Stack:** Rust, serde, existing schema renderer helpers, Python loopback fixtures, pinned sing-box 1.14.2.

**Spec:** `docs/superpowers/specs/2026-09-30-relay-generation-contract-design.md`

## Global Constraints

- Implement in the real repository checkout on `codex/relay-generation-contract`, based on PR #3.
- Only sing-box 1.14.2. No compatibility branches, production credentials or deployment.
- Schema, compiler and fixture workers own disjoint files; serialize cargo commands in the cached Linux container.
- Record meaningful failing tests before implementation, then passing results. Do not replace compiler route rules in the real kernel fixture.
- Existing `relay::config` and production reconcile/watchdog behavior remain intact.
- Each implementation receives independent contract and quality review; resolve material findings before PR creation.

## Review Focus

Confirm UUID identity across bank reuse, partial publication recovery, strict corruption handling, boot default references, unknown observations, incomplete SOCKS handshake visibility, and preservation of routing order. Distinguish a pure state protocol from a durable production driver. Reject a claim of complete drain based solely on Clash connection counts.

## Task 1: Define bank resources and strict publication protocol

**Files:** create `crates/bui-schema/src/relay_generation.rs`; export it from `crates/bui-schema/src/lib.rs`.

- [x] Write resource allocation tests, observe failure, then provide shared `Bank` port/API/member-tag methods.
- [x] Write transaction tests for immutable leases, monotonic IDs, preparing/ready/publication, partial and unknown observations, strict round trips/corruption, log evidence binding, and reclaim guards.
- [x] Implement validated state transitions and `from_json`; reject invalid snapshots rather than defaulting.
- [x] Run `cargo test --offline --locked -p bui-schema relay_generation::tests` in the cached Linux container; record RED and GREEN evidence.
- [x] Independently review state invariants and recovery boundaries.

## Task 2: Compile stable front and generation backends

**Files:** create `crates/bui-schema/src/render/relay/generation.rs`; add child module export in `crates/bui-schema/src/render/relay.rs`.

**Shared interface:** `FrontOpts { relay: RelayOpts, bank: Bank }`, `BackendOpts { bank: Bank, generation: u64, server_ip: Option<String> }`; fallible `front_config`, `backend_config`, `egress_tag`; `front_structural_hash`; strict `parse_egress_tag`.

- [x] Write behavior tests for stable front during credentials/auto/ports updates, structural changes, selector defaults, rule order, disabled/empty groups, capacity and canonical tags; observe failure.
- [x] Implement using existing routing helpers and central `Bank` resources; preserve current production renderer.
- [x] Run `cargo test --offline --locked -p bui-schema render::relay::generation::tests`; record RED and GREEN evidence.
- [x] Independently review policy parity and all hash exclusions.

## Task 3: Exercise actual generation topology

**Files:** create `crates/bui-schema/tests/relay_generation_kernel.rs` and `crates/bui-schema/tests/fixtures/relay/generation.py`.

- [x] Consume Rust compiler output for two UUIDs/two slots. Relocate only external loopback resources for test isolation.
- [x] Verify an invalid/missing bank fails as a negative control.
- [x] Exercise 50 old TCP and 50 old UDP associations, candidate preparation, partial and complete switching, new-flow routing, UDP multi-target and half-close delayed response.
- [x] Inspect actual connection chains and demonstrate the partial greeting blind spot. Close old flows and confirm B succeeds after A stops.
- [x] Run `cargo test --offline --locked -p bui-schema --test relay_generation_kernel -- --nocapture`; missing kernel may follow local skip convention, CI rejects skipped real kernel tests.
- [x] Independently review fixture assertions against actual routing rather than a replacement hand-built topology.

## Task 4: Verify and submit the foundation

**Files:** update this plan's checkboxes after verification; no production driver files.

- [x] Run host `cargo fmt --all -- --check`; Linux offline locked full workspace tests; explicit ignored UDP fixture; clippy workspace/all targets with warnings denied.
- [x] Review complete branch and resolve material findings; record remaining production prerequisites in the spec.
- [ ] Run GitNexus staged change detection; commit exact reviewed files; refresh the repository index.
- [ ] Push branch, create a stacked PR targeting `codex/relay-activation-lifecycle`, attach it to the current chat, and verify all CI checks.
- [ ] Report concrete delivered contracts, test evidence and production integration boundary. Continue architecture work without claiming this foundation alone fixes all production interruptions.
