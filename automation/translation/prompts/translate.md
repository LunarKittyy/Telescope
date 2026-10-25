You're translating UI texts of Telescope (an app that streams a phone's camera to a computer as a webcam) into {LANGUAGE}.

Read only the files in this folder: `todo.json` (the texts) and `glossary.md` (terms and style you must follow). Don't look anywhere else, and don't run anything.

Each unit in todo.json has:
- `en` (and `en_plural` for a plural): the English
- `note`: what it is, where it shows, or a length limit. Follow it.
- `ctx`: why this English has its own entry. `search` units are hidden search keywords: give the {LANGUAGE} words a person might type, then keep the English words too.
- `code`: the line of code it's on, which tells a button from a heading
- `was`: an older translation. Keep it unless the English changed its meaning.

Rules:
- Keep every `{field}`, `%1$s`, `%d` and `%%` exactly, and keep HTML tags and links as they are. Words may move around them.
- Same ending punctuation as the English, same line breaks, about the same length. A button or label longer than about 1.5 times the English may be cut off, so say it shorter.
- Plurals: give a list with the forms {LANGUAGE} needs ({PLURAL_FORMS}).
- If the glossary seems wrong for a text, still follow it and say why in an objection.

Write two files:
- `out.json`: `{"<id>": "translation"}` for every unit, or `{"<id>": ["form", "form"]}` for plurals. JSON only.
- `objections.md`: one line per unit you were unsure of or think the glossary gets wrong (`<id>: why`). Leave it empty if there are none.
