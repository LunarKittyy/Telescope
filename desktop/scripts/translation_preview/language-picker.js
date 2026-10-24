const fold = text => text.normalize('NFD').replace(/\p{Diacritic}/gu,'').toLocaleLowerCase();
let nextId = 0;
export function createLanguageFields(names, nativeMount, otherMount, saved = {}, options = {}) {
  const choices = Object.entries(names).filter(([code]) => !(options.exclude || []).includes(code)).map(([code,name]) => {
    let native = name;
    try { native = new Intl.DisplayNames([code],{type:'language'}).of(code); } catch {}
    return {code,name,native,search:fold(`${code} ${name} ${native}`)};
  });
  const fields = [];
  let nativeField;
  const selectedCodes = () => [nativeField,...fields].filter(Boolean).map(field => field.code).filter(Boolean);
  function makeField(mount, label, initial, removable = false) {
    const id = `language-field-${++nextId}`, shell = document.createElement('div'); shell.className = 'language-picker';
    const input = document.createElement('input'); input.type = 'text'; input.id = id; input.autocomplete = 'off';
    input.placeholder = 'Find a language…'; input.setAttribute('aria-label',label); input.setAttribute('role','combobox');
    input.setAttribute('aria-autocomplete','list'); input.setAttribute('aria-expanded','false'); input.setAttribute('aria-controls',`${id}-options`);
    const list = document.createElement('div'); list.id = `${id}-options`; list.className = 'language-options'; list.setAttribute('role','listbox'); list.hidden = true;
    shell.append(input,list); mount.append(shell);
    const current = choices.find(choice => choice.code === initial?.replace('_','-'));
    const field = {code:current?.code || '',input,shell}; input.value = current?.name || '';
    let matches = [], active = -1;
    const close = () => { list.hidden = true; input.setAttribute('aria-expanded','false'); input.removeAttribute('aria-activedescendant'); active = -1; };
    const commit = choice => {
      field.code = choice.code; input.value = choice.name; input.setCustomValidity(''); close();
      if (removable) remove.setAttribute('aria-label',`Remove ${choice.name}`);
      if (removable) remove.hidden = false;
      ensureNext();
    };
    function show() {
      const query = fold(input.value), used = selectedCodes();
      matches = choices.filter(choice => (choice.code === field.code || !used.includes(choice.code)) && (field.code && input.value === names[field.code] || choice.search.includes(query)));
      const rank = choice => [choice.code,choice.name,choice.native].some(value => fold(value) === query) ? 0 : fold(choice.name).startsWith(query) ? 1 : 2;
      matches.sort((a,b) => rank(a) - rank(b) || a.name.localeCompare(b.name));
      list.replaceChildren(); active = -1; input.removeAttribute('aria-activedescendant');
      for (const [index,choice] of matches.entries()) {
        const option = document.createElement('div'); option.id = `${id}-option-${index}`; option.setAttribute('role','option');
        option.setAttribute('aria-selected',String(choice.code === field.code)); option.className = 'language-option';
        const name = document.createElement('span'); name.textContent = choice.name; option.append(name);
        if (choice.native !== choice.name) { const native = document.createElement('small'); native.textContent = choice.native; option.append(native); }
        option.onpointerdown = event => event.preventDefault(); option.onclick = () => commit(choice); list.append(option);
      }
      if (!matches.length) { const empty = document.createElement('p'); empty.textContent = 'No matching languages'; list.append(empty); }
      list.hidden = false; input.setAttribute('aria-expanded','true');
    }
    input.onfocus = show;
    input.oninput = () => { field.code = ''; input.setCustomValidity(''); if (remove) remove.hidden = !input.value; show(); };
    input.onblur = () => {
      if (!shell.isConnected) return;
      if (input.value && !field.code) { const exact = choices.find(choice => fold(choice.name) === fold(input.value) || choice.code === input.value); if (exact && !selectedCodes().includes(exact.code)) commit(exact); else input.setCustomValidity('Choose a language from the list.'); }
      if (!input.value) field.code = '';
      close();
    };
    input.onkeydown = event => {
      if (event.key === 'Escape') { close(); return; }
      if (['ArrowDown','ArrowUp'].includes(event.key)) {
        event.preventDefault(); if (list.hidden) show(); if (!matches.length) return;
        active = active < 0 ? (event.key === 'ArrowDown' ? 0 : matches.length - 1) : (active + (event.key === 'ArrowDown' ? 1 : -1) + matches.length) % matches.length;
        for (const option of list.children) option.classList.remove('active');
        list.children[active].classList.add('active'); input.setAttribute('aria-activedescendant',list.children[active].id);
        list.children[active].scrollIntoView({block:'nearest'});
      } else if (event.key === 'Enter' && !list.hidden) { event.preventDefault(); if (matches[active < 0 ? 0 : active]) commit(matches[active < 0 ? 0 : active]); }
    };
    let remove;
    if (removable) {
      remove = document.createElement('button'); remove.type = 'button'; remove.textContent = 'Remove'; remove.className = 'remove-language';
      remove.setAttribute('aria-label',current ? `Remove ${current.name}` : 'Remove language');
      remove.hidden = !current;
      remove.onclick = () => { const index = fields.indexOf(field); fields.splice(index,1); shell.remove(); ensureNext(); (fields[Math.min(index,fields.length - 1)]?.input || nativeField.input).focus(); };
      shell.append(remove);
    }
    return field;
  }
  function ensureNext() {
    if (fields.length < 10 && !fields.some(field => !field.code)) fields.push(makeField(otherMount,`Other language ${fields.length + 1}`,'',true));
    fields.forEach((field,index) => field.input.setAttribute('aria-label',`Other language ${index + 1}`));
  }
  nativeMount.replaceChildren(); otherMount.replaceChildren();
  nativeField = makeField(nativeMount,'Native language',saved.nativeLanguage);
  nativeField.input.id = options.id || 'native-language-input';
  for (const code of (saved.languages || []).slice(0,10)) fields.push(makeField(otherMount,`Other language ${fields.length + 1}`,code,true));
  ensureNext();
  return {value() {
    for (const field of [nativeField,...fields]) {
      if (field.input.value && !field.code) { field.input.setCustomValidity('Choose a language from the list.'); field.input.reportValidity(); throw Error('Choose a language from the list.'); }
    }
    return {nativeLanguage:nativeField.code,languages:fields.map(field => field.code).filter(Boolean)};
  }};
}
