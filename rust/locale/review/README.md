# Translation review records

One JSON file per language, from the translation workbench: a hosted editor that drew Telescope's Qt screens and collected wordings from contributors. It went with the Python app, and nothing in this repository reads these files now. They keep who approved which wording.

- `machine` records the fingerprints of the initial machine translations. It grants no approval.
- `contributions` records the author and fingerprint of imported or accepted human wording.
- `approvals` records the reviewer, timestamp and fingerprint of explicitly approved wording.

A fingerprint covers the source text, context, plural, notes and all translated forms. An approval counts only when that fingerprint still matches a complete, non-fuzzy translation. Source locations are left out so approvals survive lines moving. A missing record means unapproved, never implicitly reviewed.
