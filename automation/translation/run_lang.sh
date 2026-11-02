#!/bin/bash
# Usage: run_lang.sh LANG "Language name" "plural note" -- a glossary, then a draft of every chunk in parallel
# $AGENT is a command that runs a model in the current folder with the prompt as its last argument
set -u
L=$1; NAME=$2; PLURALS=$3
KIT="$(cd "$(dirname "$0")" && pwd)"
W=${TR_DIR:-$KIT/work}/$L  # needs $W/todo.json from: kit.py todo $L --all -o $W/todo.json
AGENT=${DRAFT_AGENT:-${AGENT:?set AGENT (or DRAFT_AGENT) to the command that runs the drafting model}}
mkdir -p $W/g
fill() { sed -e "s/{LANGUAGE}/$NAME/g" -e "s/{PLURAL_FORMS}/$PLURALS/g" "$1"; }
if [ ! -s $W/g/glossary.md ]; then
  cp "$KIT/glossary/sv.md" $W/g/glossary_sv.md
  (cd $W/g && $AGENT "$(fill $KIT/prompts/glossary.md)" > $W/g.log 2>&1)
fi
[ -s $W/g/glossary.md ] || { echo "$L: no glossary"; exit 1; }
cp $W/g/glossary.md $KIT/glossary/$L.md
python3 -c "
import json; u=json.load(open('$W/todo.json')); n=4
size=-(-len(u)//n)
for i in range(n): json.dump(u[i*size:(i+1)*size], open(f'$W/todo.{i}.json','w'), ensure_ascii=False, indent=1)
"
for i in 0 1 2 3; do
  (
    D=$W/c$i; mkdir -p $D
    cp $W/todo.$i.json $D/todo.json; cp $W/g/glossary.md $D/glossary.md
    [ -s $D/out.json ] || (cd $D && $AGENT "$(fill $KIT/prompts/translate.md)" > $W/c$i.log 2>&1)
    [ -s $D/out.json ] && echo "$L chunk $i ok" || echo "$L chunk $i FAILED"
  ) &
done
wait
python3 -c "
import json,glob
out={}
for f in sorted(glob.glob('$W/c*/out.json')):
    try: out.update(json.load(open(f)))
    except Exception as e: print('bad json', f, e)
json.dump(out, open('$W/out.json','w'), ensure_ascii=False, indent=1)
print('$L', len(out), 'translated of', len(json.load(open('$W/todo.json'))))
"
