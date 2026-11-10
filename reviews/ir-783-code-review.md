---
id: SR-003
title: "Code review — process-wrap standard Job Object kill-on-close tests"
type: SpecReview
analysis: code-review
scope: "watchexec/process-wrap; tests/std_windows/job_handle.rs, src/std/job_object.rs, src/windows.rs, Cargo.toml, .github/workflows/test.yml; IR-783"
review_set: subset
---

## Summary

Reviewed IR-783 PR #75's test-only diff. The new tests exercise the opt-in close behavior and the suspended-assignment failure path, but the standard Job Object default lacks a negative-control test. The release and downstream-consumer acceptance checks are outside this PR's diff and remain open for ticket completion.

## Verdict

**CONDITIONAL** — the default-behavior test gap and a marker-file polling race should be fixed before treating the test change as complete.

## Findings

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-001 | medium | The standard Job Object default has no negative-control test: the existing test retains a cloned handle and explicitly terminates the job, so a default kill-on-close regression could pass. | tests/std_windows/job_handle.rs:212-227 |
| FND-002 | low | `wait_for_marker` treats file existence as completion even though the writer creates the file before writing its bytes, allowing a transient empty read. | tests/std_windows/job_handle.rs:114-125,177-180 |

## Coverage

- Reviewed ticket acceptance checks 1–4. AC-1 is exercised by the opt-in descendant test; AC-2 is exercised by the suspended marker and setup-failure tests. The test diff does not establish AC-3's unchanged default behavior or AC-4's published release and downstream consumption.
- The repository has no `spec/` tree or local `AGENTS.md`, `CLAUDE.md`, or `CONTRIBUTING.md`; the Linear ticket is the acceptance source for this review. No source or CI workflow file changed.
- The Rust lane reviewed test determinism, ownership of Windows handles, suspended assignment ordering, setup-failure cleanup, and the assertions' relation to the ticket acceptance checks. No aggregate gate was run in this review pass.
