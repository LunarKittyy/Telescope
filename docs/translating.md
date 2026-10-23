# Translating Telescope

Both apps keep every text they show in one file per language. English lives in the code and in the source files below, and anything a translation leaves out shows in English, so a language can ship half done.

| | Source (English) | A translation |
|---|---|---|
| Phone app | `android/app/src/main/res/values/strings.xml` | `android/app/src/main/res/values-<lang>/strings.xml` |
| Desktop app | `desktop/telescope/locale/telescope.pot` | `desktop/telescope/locale/<lang>.po` |

`<lang>` is a language code: `sv`, `de`, `fr`. For a regional variant, Android writes `values-pt-rBR` and the desktop writes `pt_BR.po`.

Both apps follow the system language. To try a desktop translation without changing yours, start it with `TELESCOPE_LANG=sv` set. On the phone (Android 13 and up), Settings > Apps > Telescope > Language lists every language the app has.

## Phone app

Copy `values/strings.xml` to `values-<lang>/strings.xml` and translate the text between the tags. Keep each `name` as it is, and leave out anything you haven't translated yet rather than copying the English. Don't copy `app_name`: it's marked `translatable="false"`.

- `%1$s` and `%1$d` are filled in by the app. Keep them, though they can move within the sentence
- `<plurals>` has one `<item>` per form your language needs (`one`, `few`, `many`, `other`...). Android's [plurals guide](https://developer.android.com/guide/topics/resources/string-resource#Plurals) lists them
- Write an apostrophe as `\'` and a percent sign before a placeholder as `%%`
- The comment above a string says where it shows or what fills it in

`./gradlew lintDebug` in `android/` catches a placeholder that doesn't match the English.

## Desktop app

Start a language with:

```
cd desktop
python scripts/i18n.py new sv
```

That writes `telescope/locale/sv.po` with every text and an empty `msgstr` under each. Fill in the `msgstr` lines and leave the rest alone. Check the `Plural-Forms` line in its header: the script knows the common languages and says when it doesn't know yours.

```
#. what a translator should know, from the code
#: telescope/plugins/connection.py:343
#, python-brace-format
msgid "Paired with {name}"
msgstr "Parkopplad med {name}"
```

- `{name}`, `{n}` and the like are filled in by the app. Keep the word inside the braces exactly as it is, though the field can move. A translation with a field the English doesn't have shows the English instead
- `msgid_plural` entries take one `msgstr[0]`, `msgstr[1]`... per plural form in your header
- `msgctxt "search"` marks the words the settings search looks for, not a sentence. Put in the words people would type in your language, and keep names like OBS, adb or VA-API
- `\n` is a line break. The preview's texts are broken by hand because that area doesn't wrap, so keep the breaks and the line lengths close to the English
- `#, fuzzy` means the English changed since it was translated. The app ignores fuzzy entries, so check the translation and remove the flag

To see what's left, and to check for broken fields:

```
python scripts/i18n.py todo sv      # the untranslated entries, ready to paste in
python scripts/i18n.py status       # how far along each language is
```

Some texts stay in English on purpose: Copy diagnostics (it goes into bug reports), the Browser camera web page, the command line, VB-Audio's two license lines. The phone's own camera, codec and microphone messages reach the desktop as codes the desktop words itself (see [the protocol](protocol.md#texts-the-desktop-words-itself)), so they're translated there.

## Translating with an AI

Hand it one file and the notes above. Things worth asking for:

- Translate `todo`'s output in chunks of a hundred entries or so, and paste the result back into the `.po`
- Keep the app's tone: short, plain and friendly, addressed to the person using it. Buttons are verbs ("Add phone", "Start streaming")
- Use one word for each thing across both apps. "Add phone" on the desktop and "Scan pairing code" on the phone are names of buttons that the other app's texts refer to
- Have a person who speaks the language read through it in the running app before it ships

## Plugins

Plugins people add can ship their own translations as `locale/<lang>.po` inside the plugin's folder. A plugin's texts are looked up there first, then in Telescope's own `<lang>.po` and then they stay in English, so a plugin never has to translate everything. [Writing a plugin](writing-plugins.md#translating-your-plugin) covers the author's side.

To translate someone's plugin, use the `.pot` in its `locale` folder or run the script on a copy of the plugin:

```
python scripts/i18n.py new sv --plugin path/to/the_plugin
python scripts/i18n.py todo sv --plugin path/to/the_plugin
```

The `.po` format is the same as above. Telescope's own translations don't change.

## For developers: adding a text

Phone: add it to `values/strings.xml` and use `getString(R.string.x)` or `@string/x`. Code that builds a message without a `Context` takes a `Strings` (see `Pairing.kt`).

Desktop: wrap it in `tr()` from `telescope.i18n`:

```python
from telescope.i18n import N_, tr, trn

label.setText(tr("Paired with {name}", name=phone.name))   # fields, never f-strings or + between pieces
label.setText(trn("{n} phone", "{n} phones", count))
TITLE = N_("Camera")                                        # at import time: mark it, then tr(TITLE) where it shows
```

A `# Translators:` comment right above the call goes into the `.po` file. When a text a keyboard shortcut can pick is translated (a segment button, a combo item), give it its English as `SegmentButton(tr("Auto"), value="Auto")` or `SHORTCUT_VALUE_ROLE`, so a saved shortcut still finds it in another language.

Then run `python scripts/i18n.py extract` and commit `telescope/locale`. `tests/test_i18n.py` fails when the template is out of date, when a `tr()` text can't be found, when its fields don't match what the call passes, or when something is translated at import time, before the language is loaded.
