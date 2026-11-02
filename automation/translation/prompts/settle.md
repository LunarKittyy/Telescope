You're settling disagreements about {LANGUAGE} translations of Telescope's UI.

`disputes.json` lists texts where two reviewers disagree. Each has the English with its note, option `a` and option `b`, and each side's reason. `glossary.md` has the agreed terms and style. Read only those two files.

For each, pick the better one for a native speaker using this app, or write a better third option. Write `settled.json`: `{"<id>": {"pick": "a" | "b" | "new", "text": "the final translation", "why": "a few words"}}`. JSON only.
