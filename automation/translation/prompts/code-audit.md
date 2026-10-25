Review only the translatable texts that this diff adds or changes, in Telescope (desktop: `tr()`/`trn()`/`N_()` in `desktop/`, phone: `android/app/src/main/res/values/strings.xml`). Run `git diff {BASE}...HEAD -- desktop android/app/src/main/res/values` and read only the files and lines it touches. Don't explore the rest of the repo.

Look for these problems, which no translator can fix:
- One English text used for two different meanings without a separate `ctx=` (for example a section name and a format name that are both "Light")
- Text built by joining pieces (`tr("Hello") + name`), or a text a translator can't reorder. It should be one `tr()` with `{fields}`.
- A text that never reaches `tr()` (an f-string, an English word from code shown to the user, a value saved under its translated text)
- A note (`# Translators:` or an XML comment) missing where a short word is ambiguous or has a length limit
- A text naming a button or card that doesn't exist under that name

Reply in 10 lines or fewer: `file:line - problem - fix`. Say "nothing" if it's clean.
