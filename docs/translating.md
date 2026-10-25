# Translating Telescope

## Helping without any tools

Spotted a text that reads wrong, or would like Telescope in your language? [Open a translation issue](https://github.com/LunarKittyy/Telescope/issues/new?template=translation.yml) and write what a text says now and what it should say. That's all it takes, no coding or setup needed.

To fix many texts at once you can edit the files right on GitHub. These are the languages so far, one file for each app:

| Language | Phone app | Desktop app |
|---|---|---|
| Spanish (first draft) | [strings.xml](https://github.com/LunarKittyy/Telescope/blob/master/android/app/src/main/res/values-es/strings.xml) | [es.po](https://github.com/LunarKittyy/Telescope/blob/master/desktop/telescope/locale/es.po) |
| Hindi (first draft) | [strings.xml](https://github.com/LunarKittyy/Telescope/blob/master/android/app/src/main/res/values-hi/strings.xml) | [hi.po](https://github.com/LunarKittyy/Telescope/blob/master/desktop/telescope/locale/hi.po) |
| Indonesian (first draft) | [strings.xml](https://github.com/LunarKittyy/Telescope/blob/master/android/app/src/main/res/values-in/strings.xml) | [id.po](https://github.com/LunarKittyy/Telescope/blob/master/desktop/telescope/locale/id.po) |
| Brazilian Portuguese (first draft) | [strings.xml](https://github.com/LunarKittyy/Telescope/blob/master/android/app/src/main/res/values-pt-rBR/strings.xml) | [pt_BR.po](https://github.com/LunarKittyy/Telescope/blob/master/desktop/telescope/locale/pt_BR.po) |
| Swedish | [strings.xml](https://github.com/LunarKittyy/Telescope/blob/master/android/app/src/main/res/values-sv/strings.xml) | [sv.po](https://github.com/LunarKittyy/Telescope/blob/master/desktop/telescope/locale/sv.po) |
| Ukrainian (first draft) | [strings.xml](https://github.com/LunarKittyy/Telescope/blob/master/android/app/src/main/res/values-uk/strings.xml) | [uk.po](https://github.com/LunarKittyy/Telescope/blob/master/desktop/telescope/locale/uk.po) |

1. Open the file for the app you want to fix and click the pencil icon at the top right of it
2. Change the translated texts. On the phone that's the part between `>` and `<`, on the desktop the `msgstr "..."` lines. Leave anything in `{curly braces}` or like `%1$s` as it is (the app fills those in)
3. Click **Commit changes** and then **Propose changes**. GitHub makes a copy for you and opens the pull request

A first draft hasn't been checked by someone who speaks the language yet (the desktop's Language list shows "(beta)" after its name). Reading one through and listing what's off in an issue is a big help even if you don't change the files.

Your language isn't in the table? Open a translation issue to ask for it, or start it yourself with the steps below.

There's also an experimental [translation preview](translation-preview.md) for trying desktop wording in real Qt widgets from a local browser editor. It covers the desktop app's screens, Settings pages, dialogs and menus.

The rest of this page is for people working on the files themselves.

## The files

Both apps keep every text they show in one file per language. English lives in the code and in the source files below, and anything a translation leaves out shows in English, so a language can ship half done.

| | Source (English) | A translation |
|---|---|---|
| Phone app | `android/app/src/main/res/values/strings.xml` | `values-<lang>/strings.xml` in [android/app/src/main/res](https://github.com/LunarKittyy/Telescope/tree/master/android/app/src/main/res) |
| Desktop app | `desktop/telescope/locale/telescope.pot` | `<lang>.po` in [desktop/telescope/locale](https://github.com/LunarKittyy/Telescope/tree/master/desktop/telescope/locale) |

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

## Checking that texts fit

A translation longer than the English can end up cut off, cut short with "…" or pushed out of its row. To find those:

```
python scripts/text_fit.py sv --shots shots    # what doesn't fit in Swedish, with pictures in shots/
```

It builds the window offscreen at the narrowest width of each of its layouts and goes through every settings tab. Each text that doesn't fit is listed with where it shows and how many pixels it needs. With `--shots` you also get a picture of each of those windows with the texts boxed in red.

To try a layout before any real translation exists, start the desktop with `TELESCOPE_LANG=pseudo`. That's a made-up language where every text is about 40% longer and accented ("[Šţàŕţ šţŕéàɱîñĝ ~~~~ ~~]"). Any plain English left on screen in it never went through `tr()`.

`tests/test_text_fit.py` runs the same check on every language and the pseudo one and fails on anything new. What's already known not to fit is listed in `tests/fixtures/text_fit_known.txt` (fixing one means taking its line out). Text widths depend on the font, so the test only runs a language where its texts are drawn in Inter (and Hindi's letters in Noto Sans Devanagari, since Inter has none). The Linux CI job installs both and other machines skip what they can't draw the same way.

## Translating with an AI

Hand it one file and the notes above. Things worth asking for:

- Translate `todo`'s output in chunks of a hundred entries or so, and paste the result back into the `.po`
- Keep the app's tone: short, plain and friendly, addressed to the person using it. Buttons are verbs ("Add phone", "Start streaming")
- Use one word for each thing across both apps. "Add phone" on the desktop and "Scan pairing code" on the phone are names of buttons that the other app's texts refer to
- Have a person who speaks the language read through it in the running app before it ships

For bigger runs, [automation/translation](../automation/translation/README.md) has the setup used for the languages so far: a glossary per language, prompts, `kit.py` for what's left and for the checks no model needs to do, and a review pack of screenshots for someone who speaks the language.

## First drafts

A translation nobody who speaks the language has checked yet can still ship, marked as a first draft. The desktop's Language list puts "(beta)" after its name, and both apps say it's a first draft (in Settings > General on the desktop, in About on the phone). Two things mark it, and both come off once someone has read it through:

- the line `"X-Telescope-Draft: yes\n"` in the header of the desktop's `<lang>.po`
- `android/app/src/main/res/values-<lang>/translation.xml` with `<bool name="translation_is_draft">true</bool>`

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
