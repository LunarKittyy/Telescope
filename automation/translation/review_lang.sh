#!/bin/bash
# Usage: review_lang.sh LANG "Language name" -- check the draft, one model reviews it, changes merged into $W/final.json
# $AGENT (or $REVIEW_AGENT) runs the reviewing model in the current folder with the prompt as its last argument
set -u
L=$1; NAME=$2
KIT="$(cd "$(dirname "$0")" && pwd)"
W=${TR_DIR:-$KIT/work}/$L
AGENT=${REVIEW_AGENT:-${AGENT:?set AGENT (or REVIEW_AGENT) to the command that runs the reviewing model}}
FOCUS="Read it as a native $NAME speaker using this app. Fix what sounds machine-translated or stiff, wrong meanings (the note and code line say what each text is: a button, a heading, a status), terms that differ from the glossary, and wrong plural forms."
R=$W/review; mkdir -p $R
cp $W/todo.json $W/out.json $R/ && cp $KIT/glossary/$L.md $R/glossary.md
python3 "$KIT/kit.py" check $L $W/out.json > $R/check.txt 2>&1
sed -e "s/{LANGUAGE}/$NAME/g" $KIT/prompts/review.md | python3 -c "import sys; print(sys.stdin.read().replace('{FOCUS}', sys.argv[1]))" "$FOCUS" > $R/prompt.md
[ -s $R/changes.json ] || (cd $R && $AGENT "$(cat prompt.md)" > $W/review.log 2>&1)
python3 - "$W" <<'PY'
import json, sys
w = sys.argv[1]
out = json.load(open(f"{w}/out.json"))
try:
    changes = json.load(open(f"{w}/review/changes.json"))
except Exception as e:
    print(w, "no usable changes.json:", e); changes = {}
for uid, c in changes.items():
    if uid in out and isinstance(c, dict) and "to" in c:
        out[uid] = c["to"]
json.dump(out, open(f"{w}/final.json", "w"), ensure_ascii=False, indent=1)
print(w.rsplit("/", 1)[1], len(changes), "changes from review")
PY
