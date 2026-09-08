# Remediation review gates

Each implementation batch in [the plan](../../plans/2026-09-08-audit-remediation.md) receives an independent GPT Sol review before the next batch starts.

Record the baseline commit, reviewed diff/commit, implementer and reviewer models, acceptance criteria, commands actually run, results, findings/follow-ups and final disposition. An approval is for source changes, not permission to deploy.

Use a separate record named `Bxx-review.md`. Findings must be concrete and severity-ranked. A reviewer should request changes for any unmet required acceptance criterion, not issue an unconditional approval with missing validation hidden in notes.

Reviewers do not modify application code. Corrections return to the implementation agent, then the reviewer rechecks the resulting diff. The lead records accepted checkpoints in the plan.
