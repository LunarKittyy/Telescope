# Translation review records

Each language's JSON file belongs with its `.po` catalogue in `telescope/locale/`. Together they preserve translation progress without Firebase.

- `machine` records the fingerprints of the initial AI translations. It grants no approval.
- `contributions` records the author and fingerprint of imported or accepted human wording.
- `approvals` records the reviewer, timestamp and fingerprint of explicitly approved wording.

A fingerprint covers the source text, context, plural, notes and all translated forms. An approval counts only when that fingerprint still matches a complete, non-fuzzy translation. Source locations are excluded to avoid losing approvals when lines move. Missing records mean unapproved, never implicitly human-reviewed.

The initial files have machine provenance and no approvals. `scripts/translation_preview.py --record-machine-baseline` is an explicit setup operation; don't run it after human work as a general refresh command.

The shared project's admin language export includes the `.po` and review JSON in one ZIP. Copy both into the checkout and commit them together. Pending suggestions and import conflicts belong in the separate whole-project backup. The `.po` format alone can't preserve the approval history.

The [workbench guide](../../docs/translation-preview.md) covers local review, project imports, conflict handling and Firebase setup.
