# Automation

Scripts and notes for the jobs around Telescope that aren't part of either app or of CI. Nothing in here ships, and the apps never import any of it.

- [translation/](translation/README.md): translating both apps with AI models without spending a week of usage limits on it, plus a review pack of screenshots for someone who speaks the language
- [release-testing/](release-testing/README.md): going through the [release checklist](../docs/release-checklist.md) against a nightly in an isolated Linux profile, a Windows VM and the Android emulator

Both lean on the repo's own tools where those already do the job: `desktop/scripts/i18n.py` for the desktop's catalogs and `desktop/scripts/text_fit.py` for texts that don't fit.
