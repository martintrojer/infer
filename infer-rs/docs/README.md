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

### Linux sessions 2026-05-14/15 and Wave 9 2026-05-18/19

- OpenSSL Linux perf wave docs:
  [`plans/OPENSSL_LINUX_PERF_ATTACK_SURFACE_2026_05.md`](plans/OPENSSL_LINUX_PERF_ATTACK_SURFACE_2026_05.md),
  [`plans/OPENSSL_LINUX_PERF_EXPERIMENT_PLAN_2026_05.md`](plans/OPENSSL_LINUX_PERF_EXPERIMENT_PLAN_2026_05.md),
  [`plans/OPENSSL_LINUX_PERF_BASELINE_RESULTS_2026_05.md`](plans/OPENSSL_LINUX_PERF_BASELINE_RESULTS_2026_05.md),
  and [`plans/OPENSSL_LINUX_PERF_POST_WAVE_2026_05.md`](plans/OPENSSL_LINUX_PERF_POST_WAVE_2026_05.md).
  The live dashboard row is in [`STATUS.md`](STATUS.md). Wave 10/11
  (2026-05-20/21) landed 8 perf/cap fixes; focused sentinel `sha512` improved
  `~29s` → `26.0s`, `md4` RSS `2.49 GiB` → `0.43 GiB`, and `passwd_main`
  wall-cap evasion was fixed (`3h+` → `1m01s`).

#### Deep-port day plans (Wave 9)

- [`plans/APPLY_POST_RECORD_POST_FOR_ADDRESS_DAYPLAN_2026_05.md`](plans/APPLY_POST_RECORD_POST_FOR_ADDRESS_DAYPLAN_2026_05.md)
  — four-phase apply-post / `record_post_for_address` port plan.
- [`plans/NONDISJDOMAIN_PORT_DAYPLAN_2026_05.md`](plans/NONDISJDOMAIN_PORT_DAYPLAN_2026_05.md)
  — PulseNonDisjDomain six-phase port plan.
- [`plans/LATENT_CYCLE_CURSOR_PORT_DAYPLAN_2026_05.md`](plans/LATENT_CYCLE_CURSOR_PORT_DAYPLAN_2026_05.md)
  — latent cycle-cursor shape and deep-port plan.

#### EqZero, const-zero, and ArrayAccess evidence

- [`plans/EQZERO_POTENTIAL_INVALID_ACCESS_SUMMARY_SIDEBAND_2026_05.md`](plans/EQZERO_POTENTIAL_INVALID_ACCESS_SUMMARY_SIDEBAND_2026_05.md)
  — summary `of_post` EqZero sideband design/evidence.
- [`plans/EQZERO_LOCAL_E2E_SHAPE_SPEC_2026_05.md`](plans/EQZERO_LOCAL_E2E_SHAPE_SPEC_2026_05.md)
  — local EqZero sideband shape spec.
- [`plans/ARRAY_ACCESS_CONST_NULL_COALESCING_2026_05.md`](plans/ARRAY_ACCESS_CONST_NULL_COALESCING_2026_05.md)
  — ArrayAccess const/null coalescing scout.
- [`plans/CONST_ZERO_REPR_DESIGN_EVIDENCE_2026_05.md`](plans/CONST_ZERO_REPR_DESIGN_EVIDENCE_2026_05.md)
  — const-zero representative design evidence.

#### Scout evidence and remeasurement

- [`plans/NPE_PER_FILE_REMEASURE_2026_05_18.md`](plans/NPE_PER_FILE_REMEASURE_2026_05_18.md)
  — NPE per-file remeasure and typed-stub regression context.
- [`plans/LATENT_INVALID_ACCESS_IMPORT_EVIDENCE_2026_05.md`](plans/LATENT_INVALID_ACCESS_IMPORT_EVIDENCE_2026_05.md)
  — latent invalid-access import evidence.
- [`plans/LATENT_UAF_EQZERO_SIDEBAND_FOLLOWUP_2026_05.md`](plans/LATENT_UAF_EQZERO_SIDEBAND_FOLLOWUP_2026_05.md)
  — latent UAF / EqZero sideband follow-up evidence.
- [`plans/MEMORY_LEAK_FUNPTR_RESIDUAL_CLASSIFICATION_2026_05_18.md`](plans/MEMORY_LEAK_FUNPTR_RESIDUAL_CLASSIFICATION_2026_05_18.md)
  — memory_leak.c and funptr.c residual classification before the final Wave 9 fixes.
- [`plans/POST_PHASE4_RESIDUAL_REMEASURE_2026_05_19.md`](plans/POST_PHASE4_RESIDUAL_REMEASURE_2026_05_19.md)
  — post-phase-4 C-suite residual remeasure.
- [`plans/FINAL_SEVEN_RESIDUALS_CLASSIFICATION_2026_05_19.md`](plans/FINAL_SEVEN_RESIDUALS_CLASSIFICATION_2026_05_19.md)
  — final-seven scout; superseded by the `133/4` dashboard but useful for residual provenance.

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
