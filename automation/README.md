# Automation

Scripts and notes for the jobs around Telescope that aren't part of either app or of CI. Nothing in here ships, and the apps never import any of it.

- [translation/](translation/README.md): translating both apps cheaply with AI models
- [release-testing/](release-testing/README.md): going through the [release checklist](../docs/release-checklist.md) against a nightly on a Windows VM and the Android emulator

Translation leans on the repo's own tool for the desktop's catalogues, `rust/tools/i18n.py`.
