# Translating Telescope with AI models

The rules for texts in the code and how a person translates by hand are in [docs/translating.md](../../docs/translating.md). This page is about having models do the bulk of the translating cheaply, and about the scripts that do the checks no model needs to do. Run everything from the repo root.

A translation run only starts when the maintainer asks for one. A PR that adds texts runs `python tools/i18n.py extract` in `rust/` and leaves the new entries untranslated, so they show in English until the next run. A PR that changes a phone text deletes that string from every `values-*/strings.xml`, so the old translation doesn't keep showing the old meaning.

## What's here

| | |
|---|---|
| `kit.py` | Lists what's left to translate in both apps as JSON, writes finished translations back, marks a language as a first draft, and runs the mechanical checks |
| `glossary/<lang>.md` | The agreed terms and style for each language, with an `Avoid:` line of words not to use |
| `glossary/<lang>.accepted.json` | Check findings someone already looked at and judged fine |
| `prompts/` | The prompts for drafting, reviewing, settling disputes, making a glossary and auditing new texts in a PR |
| `run_lang.sh`, `review_lang.sh` | A whole new language in one go: glossary, four drafts in parallel, then one review |

`kit.py` and `rust/tools/i18n.py` split the work like this. `i18n.py` owns the desktop's catalogues (`extract`, `new`, and `todo`/`status` for a person pasting into a `.po`), and `kit.py todo` runs its `extract` first. `kit.py` covers both apps at once and speaks JSON with an id per text, which is what a model gets and gives back.

## The rules that keep it cheap

1. **Work on the delta.** `kit.py todo <lang>` lists only new texts, fuzzy ones and phone texts whose English changed since the translation was last committed. Most runs are a handful of texts, and those need one model, not a team.
2. **Scripts before models.** `kit.py check` catches fields, markup, punctuation, line breaks, likely clipping, banned glossary words and the same English translated two ways. Never ask a model to do these.
3. **Isolate the models.** Never let a model explore the repo to work out widths, card names or placeholders: each unit already carries its code line. Copy `todo.json`, `glossary/<lang>.md` and one prompt from `prompts/` into an empty folder and run the model there with no repo access. It can't wander off reading code, and a model that only drafts text never gets to touch code at all.
4. **Reviewers return changes only**, as `{id: {to, why}}`. Never full files.
5. **Only disputes go to a third model.** If the reviewer changed something the drafter objected to, or the drafter's objection contradicts the glossary, that unit goes in `disputes.json`. Everything else is settled. Keep the orchestrator to check output, objections and disputes, never whole dumps.
6. **The cheapest model that's good at the job.** See below. Step up a tier only when the cheaper output was actually bad, not in advance.
7. **Remember what's been looked at.** `kit.py check --accept` records findings a person or model judged fine in `glossary/<lang>.accepted.json`. Later runs only show new ones.

## Who does what

Picks as of October 2026. When the models change keep the shape: a fast model with good language knowledge drafts, a model with a good ear reviews prose, a careful one looks only at the units with fields and plurals.

| Job | Default | Step up to | Why |
|---|---|---|---|
| Draft a few texts (an update) | Gemini 3.8 Flash, medium | Gemini 4 Argon, low | Language knowledge matters most, and with a fixed glossary the small model is enough |
| Draft a whole new language | Gemini 4 Argon, low | Argon, medium | Same, but more prose |
| Prose review (does it sound native) | Claude Sonnet 5.5 | Claude Opus 5.5 | Good ear for natural phrasing, cheaper than Opus |
| Technical review (only units the check flagged, or with fields, plurals or markup) | `kit.py check`, then GPT-6 Luna, low | GPT-6.1 Sol, medium | Catches tiny details and only needs the flagged units |
| Settle disputes | Gemini 4 Argon, medium | Argon, high | Best at the language, and it only sees a short list |
| Code audit of new texts (wrong ctx, joined strings, missing notes) | GPT-6.1 Sol, medium, read-only, `prompts/code-audit.md` | none | Only when a PR adds or changes texts, and only on that diff. Never part of a translation run |
| Orchestrator | Claude Sonnet 5.5 for updates | Claude Opus 5.5 for a new language | Mostly runs scripts and merges JSON |

None of this needs the biggest models at high effort.

## Recipe: an update (new or changed texts in a language that exists)

```bash
python automation/translation/kit.py todo sv -o work/todo.json   # nothing listed: stop here
```

1. **Under about 15 texts:** the orchestrator translates them itself with `glossary/sv.md`, then runs `check`. No other model needed.
2. **More than that:** set up a work folder outside the repo with `todo.json`, `glossary.md` and `prompts/translate.md` (with `{LANGUAGE}` and `{PLURAL_FORMS}` filled in), and run the drafting model there. It writes `out.json` and `objections.md`.
3. `python automation/translation/kit.py check sv work/out.json > work/check.txt`
4. If there's prose (sentences, not just labels) or `check.txt` isn't empty, have one reviewer go over it with `prompts/review.md`. Pick the reviewer by what's in it: prose goes to the prose reviewer, fields and plurals to the technical one. Don't use both unless both kinds are there, and give each only its units.
5. Merge the changes into `out.json`. Read `objections.md` before going on, since `review_lang.sh` doesn't pass it to the reviewer. Build `disputes.json` from the objections and from the reviewer's changes to the same units, and give it to the settling model with `prompts/settle.md`. Many objections about one term mean the glossary has it wrong: fix the glossary, then settle every text that uses the term, old ones included.
6. `kit.py apply sv work/out.json`, then `kit.py check sv`, and look at the cards in the language (`TELESCOPE_LANG=sv`, see the real-device step below) if any label or button changed.
7. Run the checks in `CONTRIBUTING.md`, then commit as "Update the Swedish translation".

## Recipe: a new language

A cheap first draft is fine. A small model drafts, one model reviews, and the language ships marked with `kit.py draft <lang> on`, which puts "(beta)" in the desktop's list and a note in both apps. Turn it off with `kit.py draft <lang> off` after the full recipe below, or once a speaker has read it through.

`run_lang.sh LANG "Name" "plural note"` does the glossary plus four parallel draft chunks, and `review_lang.sh LANG "Name"` checks the result and runs one review over it. Both run whatever command is in `AGENT` (or `DRAFT_AGENT` and `REVIEW_AGENT` to use two models) inside an isolated work folder, with the prompt as its last argument. The work folders go in `automation/translation/work/` unless `TR_DIR` says otherwise, and that folder is ignored by git.

1. `cd rust && python tools/i18n.py new <lang>`. Check the Plural-Forms line, and on Android which quantities (`one`, `few`, `many`, `other`) the language uses.
2. **Glossary:** give the drafting model the English column and style section of `glossary/sv.md` as the term list (it doesn't need to read all the texts) and ask for `glossary/<lang>.md` in the same shape, with an `Avoid:` line. One other model critiques only that file and the orchestrator settles it. If someone on the project speaks the language, they get the last word.
3. `kit.py todo <lang> --all -o work/todo.json` and split it into chunks of about 300 units:
   ```bash
   python -c "import json,sys; u=json.load(open('todo.json')); [json.dump(u[i:i+300], open(f'todo.{i//300}.json','w'), ensure_ascii=False) for i in range(0, len(u), 300)]"
   ```
   Draft every chunk in parallel, each in its own folder.
4. Check each chunk, review it (prose chunks to the prose reviewer, the flagged units to the technical one), then read the objections and settle disputes as in the update recipe.
5. Apply, run `check` until it's clean, then run the desktop in the language (`TELESCOPE_LANG=<lang>`, see the next step) and look for clipped labels and buttons. Fix clipping by shortening the text first and only change the layout when the shortest good wording still doesn't fit.
6. Real-device check: `./gradlew installDev` in `android/`, then `adb shell cmd locale set-app-locales com.telescope.dev --locales <lang>`. On the desktop pick the language under Settings > General > Language (or run with `TELESCOPE_LANG=<lang>`). The dev desktop is `cargo run -p telescope-app -- dev --keep` in `rust/`.

## Good to know

- Run agent CLIs headless with `< /dev/null` at the end (some wait on stdin otherwise), and give them write access to the work folder only.
- If a new English text duplicates another but means something else, the code needs a context (`tr_ctx`), not a cleverer translation. Raise it as a code fix.
- Folded cards are saved under their English (`tr()` returns text that remembers its English), so two cards may share a translation without breaking anything.
- The phone sends its camera, codec and microphone messages as codes and the desktop words them in its own catalog. Third-party plugins ship their own catalogs. `docs/translating.md` covers both.
- Check every factual claim in a model's objections (a button name, how Windows labels something) before acting on it. Models make things up when they're unsure.
- A full-language prose review takes about 180k tokens. If the usual reviewer's limits run out, point `REVIEW_AGENT` at the settling model.
- Don't write a heading as a plural when its "one" form has no number in it. Android lint fails it in languages where "one" also covers 0 or 21 (Hindi, Portuguese, Ukrainian), so use two plain strings instead.
- Drafting a whole language in four parallel chunks takes a small model about 10 minutes. Expect a handful of texts per language to come out too long for the window.
