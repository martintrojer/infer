# infer-rs documentation

Current status and active work are intentionally separated:

- Current dashboard: [`STATUS.md`](STATUS.md)
- Active tasks/backlog: `mu state -w infer-rs` and
  `mu task list -w infer-rs --status OPEN`
- Historical investigations: [`plans/`](plans/)

## Start here

- [`../README.md`](../README.md) — project quickstart and CLI basics.
- [`STATUS.md`](STATUS.md) — current correctness and performance dashboard.
- [`TESTING.md`](TESTING.md) — test, benchmark, and tracing workflows.
- [`../AGENTS.md`](../AGENTS.md) — coding-agent rules and debugging recipes.

## Architecture references

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — Infer/Rust-port architecture overview.
- [`FRONTEND.md`](FRONTEND.md) — capture/front-end pipeline.
- [`BACKEND.md`](BACKEND.md) — analysis scheduling and backend pipeline.
- [`CHECKERS.md`](CHECKERS.md) — Infer checker overview.
- [`PULSE.md`](PULSE.md) — Pulse crate/design notes.

## IR and pipeline references

- [`SIL.md`](SIL.md) — Smallfoot Intermediate Language notes.
- [`TEXTUAL.md`](TEXTUAL.md) — Textual IR format and lowering notes.
- [`STORE_TEXTUAL.md`](STORE_TEXTUAL.md) — store/export pipeline and accepted
  fidelity limits.

## Triage and parity tracks

- [`triage/c_pulse_summary_mismatches_2026_05_11.md`](triage/c_pulse_summary_mismatches_2026_05_11.md)
  — C-suite OCaml↔Rust Pulse summary mismatch triage. Per-cluster status
  and remeasure totals after the initial cluster pass.

## Archived plans and findings

Files under [`plans/`](plans/) are historical records, not the live task list.
If an archive contains actionable work, that work should have a corresponding
`mu` task. Strategic themes and migrated task IDs are summarized in
[`STATUS.md`](STATUS.md).

- [`plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`](plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md)
  — OpenSSL performance archive.
- [`plans/CONVERGENCE_8D4V_FINDINGS.md`](plans/CONVERGENCE_8D4V_FINDINGS.md)
  — retained-state decomposition.
- [`plans/CONVERGENCE_NEXT_STEPS.md`](plans/CONVERGENCE_NEXT_STEPS.md) — early
  convergence/structural-sharing plan.
- [`plans/STRUCTURAL_SHARING_PROTOTYPE.md`](plans/STRUCTURAL_SHARING_PROTOTYPE.md)
  — structural-sharing prototype plan.
