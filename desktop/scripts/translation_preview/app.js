const $ = id => document.getElementById(id);
let data, image, selected, generation = 0, timer, busy = false;
let drafts = {}, reviewed = {}, lang = 'es';
let shared, sharedUser = null, sharedRole = null, submitting = false, adminUi, preferredKey;
let repositoryData, cloudOverrides = {};
let selectedSubmission, sharedState;
let profileFields;
let localLanguages = [], languageCreator;
const storageKey = 'telescope-translation-preview-v1';
try { const saved = JSON.parse(localStorage.getItem(storageKey) || '{}'); drafts = saved.drafts || {}; reviewed = saved.prepared || saved.reviewed || {}; lang = saved.lang || lang; localLanguages = saved.localLanguages || []; } catch {}
const currentDrafts = () => ({...(cloudOverrides[lang] || {}),...Object.fromEntries(Object.entries(drafts[lang] || {}).filter(([k]) => data?.entries[k]))});
const original = k => lang === 'en' ? data.entries[k].english : (data.translations[lang]?.[k] || '');
const value = k => currentDrafts()[k] ?? original(k);
const reviewStamp = k => JSON.stringify([value(k), data.revision]);
const humanChecked = k => Boolean(value(k)) && Boolean(data.approvals?.[lang]?.[k]) && value(k) === original(k);
const prepared = k => Boolean(value(k)) && reviewed[lang]?.[k] === reviewStamp(k);
// Codes and keys come from browser storage and the address bar, so writes skip names that reach Object.prototype
function setDraft(store, code, key, text) {
  if (code === '__proto__' || code === 'constructor' || code === 'prototype' || key === '__proto__' || key === 'constructor' || key === 'prototype') return;
  if (!Object.hasOwn(store, code)) store[code] = {};
  store[code][key] = text;
}
function dropDraft(store, code, key) {
  if (code === '__proto__' || code === 'constructor' || code === 'prototype' || key === '__proto__' || key === 'constructor' || key === 'prototype') return;
  if (Object.hasOwn(store, code)) delete store[code][key];
}
const save = () => { try { localStorage.setItem(storageKey, JSON.stringify({drafts, prepared:reviewed, lang,localLanguages})); } catch { $('draft-status').textContent = 'Browser storage is unavailable. Download your draft before closing.'; } };
function addLocalLanguage(code) {
  if (data.languages.includes(code)) return;
  data.languages.push(code); data.translations[code] = {}; data.approvals[code] = {};
  data.language_stats[code] = {total:data.total,translated:0,approved:0,machine:0};
}
function sceneKeys() { return image ? [...new Set(image.regions.map(r => r.key))].filter(k => data.entries[k]) : (data?.coverage.scenes[$('scene').value] || []).filter(k => data.entries[k]); }
function updateProgress() {
  const keys = sceneKeys(), done = keys.filter(prepared).length;
  $('progress').textContent = `${done} / ${keys.length} ready`;
  $('export').disabled = lang === 'en' || Boolean($('error').textContent) || busy;
  $('approve').hidden = data?.mode !== 'admin' && !['owner','admin'].includes(sharedRole);
  $('approve').disabled = busy || !image || !value(selected) || Boolean($('error').textContent) || (sharedUser && !shared?.username);
  $('submit').hidden = !sharedUser || data?.mode === 'admin' || !$('approve').hidden;
  $('review').classList.toggle('primary', $('approve').hidden && $('submit').hidden);
  $('submit').disabled = submitting || busy || !image || !value(selected) || Boolean($('error').textContent);
  $('review').disabled = lang === 'en' || Boolean($('error').textContent) || busy || !image || !value(selected);
}
function rows() {
  const search = $('search').value.toLocaleLowerCase();
  const keys = sceneKeys().filter(k => `${data.entries[k].english} ${value(k)}`.toLocaleLowerCase().includes(search));
  $('texts').replaceChildren(); $('empty').hidden = keys.length > 0;
  for (const k of keys) {
    const button = document.createElement('button'); button.className = 'text-row'; button.setAttribute('aria-pressed', String(k === selected));
    const title = document.createElement('span'); title.textContent = value(k) || data.entries[k].english;
    const note = document.createElement('small'); const done = humanChecked(k);
    note.textContent = `${done ? 'Approved · ' : prepared(k) ? 'Ready for export · ' : ''}${data.entries[k].english}`; note.className = done ? 'reviewed' : '';
    button.append(title, note); button.onclick = () => choose(k); $('texts').append(button);
  }
  updateProgress();
}
function highlight() {
  $('regions').replaceChildren();
  for (const r of image?.regions || []) {
    if (r.slot === 'tooltip' && r.key !== selected) continue;
    const b = document.createElement('button'); b.className = `region${r.key === selected ? ' selected' : ''}`;
    b.tabIndex = -1; b.setAttribute('aria-label', data.entries[r.key]?.english || r.shown);
    b.title = r.shown;
    const [x,y,w,h] = r.rect;
    Object.assign(b.style, {left:`${100*x/image.width}%`,top:`${100*y/image.height}%`,width:`${100*w/image.width}%`,height:`${100*h/image.height}%`});
    b.onclick = () => choose(r.key); $('regions').append(b);
  }
}
function choose(k) {
  if (!data.entries[k]) return;
  selected = k; $('edit').hidden = false;
  const entry = data.entries[k]; $('english').textContent = entry.english;
  $('translation').value = value(k); $('translation').disabled = lang === 'en';
  $('translation').lang = lang.replace('_','-');
  $('target-label').textContent = languageName(lang);
  $('context').textContent = [...entry.comments, entry.context ? `Context: ${entry.context}` : '', entry.locations.join(' · ')].filter(Boolean).join('\n');
  const uses = (image?.regions || []).filter(r => r.key === k);
  $('fields').textContent = uses.find(r => Object.keys(r.values).length) ? `Example values: ${JSON.stringify(uses.find(r => Object.keys(r.values).length).values)}` : 'An empty translation falls back to English.';
  $('placement').textContent = uses.some(r => r.slot === 'tooltip') ? `Tooltip on the highlighted control: ${uses[0].shown}` : '';
  rows(); highlight();
}
async function post(path, extra = {}) {
  const response = await fetch(path, {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({lang,scene:$('scene').value,drafts:currentDrafts(),revision:data.revision,...extra})});
  if (!response.ok) { let message; try { message = (await response.json()).error; } catch {} throw Error(message || 'The preview could not be generated.'); }
  return response;
}
async function render() {
  const request = ++generation; busy = true;
  $('render-status').textContent = 'Rendering your draft in Qt…'; $('picture').setAttribute('aria-busy','true'); updateProgress();
  try {
    const response = await post('/api/render'); const result = await response.json();
    if (request !== generation) return;
    image = result;
    const picture = document.createElement('img'); picture.id = 'preview';
    picture.alt = 'The Phone card rendered by Telescope'; picture.src = `data:image/png;base64,${image.image}`;
    $('preview-mount').replaceChildren(picture);
    $('error').textContent = '';
    $('render-status').textContent = 'Live Qt render';
    $('fit').textContent = image.findings.map(f => `${f.kind}: ${f.text}`).join('\n');
    if (preferredKey && sceneKeys().includes(preferredKey)) selected = preferredKey;
    preferredKey = null;
    if (!sceneKeys().includes(selected)) selected = sceneKeys()[0];
    if (selected) choose(selected); else { rows(); highlight(); $('edit').hidden = true; }
  } catch (error) {
    if (request !== generation) return;
    $('render-status').textContent = image ? 'Preview unavailable. Showing the last successful render.' : 'Preview unavailable. Use Refresh to try again.';
    $('error').textContent = error.message;
  } finally {
    if (request === generation) { busy = false; $('picture').setAttribute('aria-busy','false'); updateProgress(); }
  }
}
$('translation').oninput = () => {
  setDraft(drafts, lang, selected, $('translation').value);
  dropDraft(reviewed, lang, selected);
  busy = true;
  save(); rows(); clearTimeout(timer); generation++; $('render-status').textContent = 'Draft changed. Preview updating…';
  timer = setTimeout(render, 650);
};
$('zoom').onclick = () => {
  const enlarged = $('zoom').getAttribute('aria-pressed') !== 'true';
  $('zoom').setAttribute('aria-pressed', String(enlarged));
  $('zoom').textContent = enlarged ? 'Fit to width' : 'Actual size';
  $('picture').parentElement.classList.toggle('actual-size', enlarged);
};
$('search').oninput = rows;
$('refresh').onclick = () => { clearTimeout(timer); render(); };
$('scene').onchange = () => { clearTimeout(timer); render(); };

$('reset').onclick = () => { dropDraft(drafts, lang, selected); dropDraft(reviewed, lang, selected); save(); choose(selected); render(); };
$('review').onclick = () => {
  if (busy || $('error').textContent || lang === 'en' || !value(selected)) return;
  setDraft(reviewed, lang, selected, reviewStamp(selected)); save();
  const keys = sceneKeys(), next = keys.find(k => !prepared(k));
  rows(); if (next) choose(next); else $('progress').textContent = `All ${keys.length} texts in this scene ready for export`;
};
$('export').onclick = async () => {
  try {
    const response = await post('/api/export'); const url = URL.createObjectURL(await response.blob());
    const a = document.createElement('a'); a.href = url; a.download = `${lang}.po`; a.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
    $('draft-status').textContent = 'Draft downloaded. Open a pull request with the .po file when you have checked it.';
  } catch (error) { $('error').textContent = error.message; }
};
$('backup').onclick = () => {
  const url = URL.createObjectURL(new Blob([JSON.stringify({drafts,prepared:reviewed,lang,revision:data.revision},null,2)],{type:'application/json'}));
  const a = document.createElement('a'); a.href = url; a.download = 'telescope-translation-session.json'; a.click(); setTimeout(() => URL.revokeObjectURL(url),1000);
};
$('restore').onclick = () => $('restore-file').click();
$('restore-file').onchange = async () => {
  const file = $('restore-file').files[0]; $('restore-file').value = '';
  if (!file) return;
  try {
    if (file.size > 4_000_000) throw Error();
    const saved = JSON.parse(await file.text());
    let count = 0;
    for (const [code, texts] of Object.entries(saved.drafts || {})) {
      if (!data.languages.includes(code) || typeof texts !== 'object' || !texts) continue;
      for (const [k, text] of Object.entries(texts)) {
        if (!data.entries[k] || typeof text !== 'string' || text.length > 4000) continue;
        setDraft(drafts, code, k, text); count++;
      }
    }
    save(); if (selected) choose(selected); rows(); render();
    $('draft-status').textContent = count ? `Restored ${count} ${count === 1 ? 'draft' : 'drafts'} from the backup.` : 'That backup has no drafts for these texts.';
  } catch { $('draft-status').textContent = 'That file isn’t a session backup from this editor.'; }
};
async function start() {
  try {
    fetch('/api/warm').catch(() => {});
    let response = await fetch('/catalog.json'); if (!response.ok) response = await fetch('/api/catalog'); if (!response.ok) throw Error('The catalog could not be loaded. Refresh to try again.'); data = await response.json();
    repositoryData = structuredClone(data); cloudOverrides = {};
    for (const code of localLanguages) addLocalLanguage(code);
    if (!data.languages.includes(lang)) lang = 'es';
    const old = Object.keys(drafts[lang] || {}).filter(k => !data.entries[k]).length;
    if (old) $('draft-status').textContent = `${old} saved drafts no longer match these texts and are excluded from previews and .po export. Download the session .json to keep them.`;
    $('scene').replaceChildren();
    $('missing').replaceChildren();
    for (const [id,title] of Object.entries(data.scenes)) $('scene').add(new Option(title,id));
    const c = data.coverage;
    $('coverage-summary').textContent = `Preview coverage${data.drift.length ? ' changed' : ''} · ${c.missing.length} missing`;
    $('coverage-count').textContent = `${data.total - c.mapped.length} catalog texts have no mapped preview in this prototype. The list below shows the Phone texts still missing.`;
    for (const k of c.missing) { const li = document.createElement('li'); li.textContent = data.entries[k]?.english || `${k} (removed from source)`; $('missing').append(li); }
    $('overview-status').textContent = '';
    $('sign-in').disabled = !data.shared_available;
    $('shared-status').textContent = data.shared_available ? 'GitHub is used for attribution. Suggestions wait for admin approval.' : 'Shared sign-in is being set up. The manual editor is available below.';
    route();
    if (data.shared_available && sessionStorage.getItem('telescope-shared-session') === 'yes') {
      try { shared = await (await import('/shared.js')).connect(accountChanged); }
      catch { $('shared-status').textContent = 'The shared project could not be reached. Your browser drafts are still available.'; }
    }
  } catch (error) { $('overview-status').textContent = error.message; }
}
const languageNames = new Intl.DisplayNames(['en'], {type:'language'});
const nativeName = code => new Intl.DisplayNames([code.replace('_','-')], {type:'language'}).of(code.replace('_','-'));
const languageName = code => languageNames.of(code.replace('_','-'));
const percent = (done, total) => !done || !total ? 0 : done === total ? 100 : Math.min(99.9, Math.max(.1, Math.round(1000 * done / total) / 10));
function completion(code) {
  const stats = data.language_stats[code];
  const edits = drafts[code] || {};
  let translated = stats.translated;
  for (const [k,text] of Object.entries(edits)) {
    if (data.entries[k] && !data.entries[k].plural) translated += Number(Boolean(text)) - Number(Boolean(data.translations[code]?.[k]));
  }
  const invalidated = Object.entries(edits).filter(([k,text]) => data.entries[k]
    && data.approvals?.[code]?.[k] && text !== data.translations[code]?.[k]).length;
  const checked = (stats.approved || 0) - invalidated;
  return {total: stats.total, translated, checked};
}
function meter(done, total, label, checked = 0) {
  const progress = document.createElement('div'); progress.className = 'completion-meter';
  progress.setAttribute('role','img');
  progress.setAttribute('aria-label', `${label}: ${checked} approved, ${done - checked} drafted, ${total - done} untranslated`);
  for (const [kind,count] of [['human',checked],['machine',done - checked]]) {
    const segment = document.createElement('span'); segment.className = kind;
    segment.style.width = `${total ? 100 * count / total : 0}%`; progress.append(segment);
  }
  return progress;
}
function localEdits(code) { return Object.entries(drafts[code] || {}).filter(([k,text]) => data.entries[k] && text !== (data.translations[code]?.[k] || '')).length; }
function languageRows() {
  const query = $('language-search').value.toLocaleLowerCase();
  $('language-list').replaceChildren();
  for (const code of data.languages.filter(l => l !== 'en').sort((a,b) => languageName(a).localeCompare(languageName(b)))) {
    const name = languageName(code), native = nativeName(code);
    if (!`${name} ${native} ${code}`.toLocaleLowerCase().includes(query)) continue;
    const stats = completion(code), row = document.createElement('a');
    row.className = 'language-row'; row.href = `#language/${code}`;
    const title = document.createElement('div'), heading = document.createElement('strong'), subtitle = document.createElement('span');
    heading.textContent = name; subtitle.textContent = native; subtitle.lang = code.replace('_','-');
    title.append(heading, subtitle);
    const progress = document.createElement('div'), number = document.createElement('span'); progress.className = 'language-progress';
    number.textContent = `${percent(stats.checked, stats.total)}% approved`;
    progress.append(meter(stats.translated, stats.total, name, stats.checked), number);
    const remaining = document.createElement('div'), count = document.createElement('span'); remaining.className = 'remaining';
    count.textContent = `${stats.translated} / ${stats.total} translated`; remaining.append(count);
    if (localEdits(code)) { const note = document.createElement('small'); note.textContent = `${localEdits(code)} local ${localEdits(code) === 1 ? 'edit' : 'edits'}`; remaining.append(note); }
    row.append(title, progress, remaining); $('language-list').append(row);
  }
  $('language-empty').hidden = $('language-list').childElementCount > 0;
}
function breadcrumbs(view) {
  const nav = $('breadcrumb'); nav.replaceChildren();
  const home = document.createElement('a'); home.href = '#languages'; home.textContent = 'Languages'; nav.append(home);
  if (view === 'modes') { nav.replaceChildren(); return; }
  if (['account','admin','add-language'].includes(view)) { const current = document.createElement('span'); current.textContent = view === 'account' ? 'Your account' : view === 'admin' ? 'Admin mode' : 'Add a language'; nav.append(current); return; }
  if (view === 'languages') { home.setAttribute('aria-current','page'); return; }
  const language = document.createElement('a'); language.href = `#language/${lang}`; language.textContent = languageName(lang); nav.append(language);
  if (view === 'language') language.setAttribute('aria-current','page');
  else { const phone = document.createElement('span'); phone.textContent = 'Phone'; phone.setAttribute('aria-current','page'); nav.append(phone); }
}
function route() {
  if (!data) return;
  const [requested, code] = location.hash.slice(1).split('/');
  const validLanguage = code && data.languages.includes(code) && code !== 'en';
  const view = requested === 'add-language' ? requested : ['account','admin'].includes(requested) && sharedUser && (requested !== 'admin' || ['owner','admin'].includes(sharedRole)) ? requested : ['language','edit'].includes(requested) && validLanguage ? requested : requested === 'modes' || !requested ? 'modes' : 'languages';
  clearTimeout(timer); generation++; busy = false;
  $('modes-view').hidden = view !== 'modes';
  $('account-view').hidden = view !== 'account'; $('admin-view').hidden = view !== 'admin';
  $('language-create-view').hidden = view !== 'add-language';
  $('account-menu').open = false;
  $('languages-view').hidden = view !== 'languages'; $('language-view').hidden = view !== 'language';
  $('editor-view').hidden = view !== 'edit'; $('export').hidden = view !== 'edit';
  if (['language','edit'].includes(view)) { lang = code; save(); }
  $('mode-label').textContent = data.mode === 'admin' ? 'Local administrator' : sharedUser ? 'Shared contribution' : 'Manual contribution';
  breadcrumbs(view);
  $('mode-link').hidden = view === 'modes';
  if (view === 'modes') { $('modes-title').focus(); }
  else if (view === 'account') { loadAccount(); $('account-title').focus(); }
  else if (view === 'admin') { openAdmin(); $('admin-title').focus(); }
  else if (view === 'add-language') { openLanguageCreator(); $('language-create-title').focus(); }
  else if (view === 'languages') { languageRows(); $('languages-title').focus(); }
  else if (view === 'language') {
    $('admin-import').hidden = data.mode !== 'admin';
    const stats = completion(lang); $('language-title').textContent = languageName(lang);
    $('language-description').textContent = nativeName(lang);
    $('language-meter').replaceChildren(meter(stats.translated, stats.total, languageName(lang), stats.checked));
    $('language-count').textContent = `${stats.checked} approved · ${stats.translated - stats.checked} drafted · ${stats.total - stats.translated} untranslated`;
    const keys = data.coverage.mapped, translated = keys.filter(k => data.translations[lang]?.[k]).length;
    $('phone-count').textContent = `${keys.filter(humanChecked).length} / ${keys.length} approved`;
    $('phone-link').href = `#edit/${lang}`;
    $('scope-note').textContent = `This prototype has previews for ${keys.length} texts in the Phone card and its heat warning. The rest of the desktop catalogue is not available in the editor yet.`;
    $('language-title').focus();
  } else {
    image = null; selected = null; $('error').textContent = ''; $('fit').textContent = ''; $('search').value = '';
    $('preview-mount').replaceChildren(); $('edit').hidden = true; $('regions').replaceChildren(); selected = sceneKeys()[0]; if (selected) choose(selected); else rows(); render();
  }
  window.scrollTo(0,0);
}
$('language-search').oninput = () => { if (data) languageRows(); };
$('approve').onclick = async () => {
  if ((data.mode !== 'admin' && !['owner','admin'].includes(sharedRole)) || busy || !value(selected)) return;
  try {
    if (data.mode === 'admin') await post('/api/approve', {identity:selected});
    else {
      if (!sharedState?.projection) throw Error('Import the repository snapshot in Admin mode first.');
      const entry = data.entries[selected];
      if (sharedState.projection.sources[entry.id] !== entry.sourceDigest) throw Error('This text changed in the shared project. Reload before approving it.');
      const submission = selectedSubmission?.entry === entry.id && selectedSubmission.language === lang ? selectedSubmission : null;
      const item = {language:lang,entry:entry.id,sourceDigest:entry.sourceDigest,text:value(selected),submission:submission?.id || '',
        author:submission ? submission.author : 'github:'+shared.username};
      $('approve').disabled = true;
      sharedApproved({...item,reviewer:await shared.approve(item)});
      if (submission) selectedSubmission = null;
    }
    dropDraft(drafts, lang, selected); save();
    if (data.mode === 'admin') await start();
    else { choose(selected); $('draft-status').textContent = 'Translation approved.'; }
  } catch (error) { $('error').textContent = error.message; updateProgress(); }
};
$('import-po').onclick = async () => {
  const file = $('import-file').files[0], author = $('import-author').value.trim();
  if (!file || !author) { $('import-status').textContent = 'Choose a .po file and enter its contributor’s public name.'; return; }
  if (file.size > 4_000_000) { $('import-status').textContent = 'Choose a .po file smaller than 4 MB.'; return; }
  $('import-po').disabled = true;
  try {
    const response = await post('/api/import-po', {content:await file.text(),author});
    const result = await response.json();
    await start();
    $('import-status').textContent = `Imported ${result.changed} changed translations for approval.`;
  } catch (error) { $('import-status').textContent = error.message; }
  finally { $('import-po').disabled = false; }
};
window.addEventListener('hashchange', route);
function accountChanged(user, role, username, identityError) {
  sharedUser = user;
  $('brand-home').href = user ? '#languages' : '#modes';
  sharedRole = role;
  adminUi = null;
  try { sessionStorage.setItem('telescope-shared-session', user ? 'yes' : ''); } catch {}
  $('account-menu').hidden = !user;
  $('admin-link').hidden = !['owner','admin'].includes(role);
  $('sign-in').textContent = user ? 'Continue to languages' : 'Sign in with GitHub';
  $('shared-status').textContent = user ? 'Signed in with GitHub. Your suggestions will wait for admin approval.' : 'GitHub is used for attribution. Suggestions wait for admin approval.';
  if (identityError) $('shared-status').textContent = identityError;
  $('mode-label').textContent = data?.mode === 'admin' ? 'Local administrator' : user ? 'Shared contribution' : 'Manual contribution';
  if (data) { updateProgress(); if (['#admin','#account'].includes(location.hash)) route(); }
  if (user) loadSharedProgress();
  else if (repositoryData) { data = structuredClone(repositoryData); cloudOverrides = {}; sharedState = null; for (const code of localLanguages) addLocalLanguage(code); if (location.hash !== '#modes') route(); }
}
// Approval documents saved after the project version was published are laid over its projection
function applyShared({projection, approvals, languages}) {
  const translations = structuredClone(projection.translations), reviewers = structuredClone(projection.approvals);
  const conflicted = new Set(projection.conflicts.map(item => `${item.language}-${item.entry}`));
  for (const code of languages) if (code !== 'en') { translations[code] ||= {}; reviewers[code] ||= {}; }
  for (const item of approvals) {
    if (projection.folded[item.id] === item.approvedAt || projection.sources[item.entry] !== item.sourceDigest || !translations[item.language] || conflicted.has(item.id)) continue;
    translations[item.language][item.entry] = item.text; reviewers[item.language][item.entry] = item.reviewer;
  }
  const total = Object.keys(projection.sources).length;
  data.language_stats = {};
  for (const code of Object.keys(translations)) data.language_stats[code] = {total,translated:Object.keys(translations[code]).length,approved:Object.keys(reviewers[code]).length,machine:projection.language_stats[code]?.machine || 0};
  data.languages = ['en',...Object.keys(translations)];
  for (const code of Object.keys(translations)) {
    data.translations[code] ||= {};
    cloudOverrides[code] = {}; data.approvals[code] = {};
    for (const [key,entry] of Object.entries(data.entries)) {
      if (projection.sources[entry.id] !== entry.sourceDigest) continue;
      const translated = translations[code][entry.id] || '';
      data.translations[code][key] = translated;
      if (translated !== repositoryData.translations[code]?.[key]) cloudOverrides[code][key] = translated;
      if (reviewers[code][entry.id]) data.approvals[code][key] = reviewers[code][entry.id];
    }
  }
}
function sharedApproved(item) {
  if (!sharedState) return loadSharedProgress();
  const id = `${item.language}-${item.entry}`;
  sharedState.approvals = [...sharedState.approvals.filter(existing => existing.id !== id),{...item,id,approvedAt:new Date().toISOString()}];
  applyShared(sharedState);
}
async function loadSharedProgress() {
  for (let attempt = 0; attempt < 3; attempt++) {
    try {
      const loaded = await shared.loadProgress();
      if (!loaded.projection || !sharedUser) return;
      const [requested, code] = location.hash.slice(1).split('/'), wasMissing = !data.languages.includes(code);
      sharedState = loaded; applyShared(loaded);
      if (['language','edit'].includes(requested) && wasMissing && data.languages.includes(code)) route();
      else if (location.hash === '#languages') languageRows();
      return;
    } catch { await new Promise(resolve => setTimeout(resolve, 1500 * (attempt + 1))); }
  }
  $('draft-status').textContent = 'Shared progress could not be loaded. Showing this checkout’s catalogue until you reload.';
}
$('sign-in').onclick = async () => {
  if (sharedUser) { location.hash = '#languages'; return; }
  $('sign-in').disabled = true;
  $('shared-status').textContent = 'Opening GitHub sign-in…';
  try {
    const module = await import('/shared.js');
    shared = await module.connect(accountChanged);
    await shared.signIn();
    location.hash = '#languages';
  } catch (error) {
    $('shared-status').textContent = error.code === 'auth/unauthorized-domain'
      ? 'This address is not authorised for GitHub sign-in. Add it in Firebase Authentication or use the manual editor.'
      : error.code === 'auth/popup-closed-by-user' ? 'Sign-in was closed. You can try again or use the manual editor.'
      : 'GitHub sign-in failed. Try again or use the manual editor.';
  } finally { $('sign-in').disabled = false; }
};
$('sign-out').onclick = async () => {
  try { await shared.signOut(); $('draft-status').textContent = 'Signed out. Your browser drafts are still available for download.'; }
  catch { $('draft-status').textContent = 'Sign-out failed. Try again before leaving this computer.'; }
};
$('manual-mode').onclick = async event => {
  if (!sharedUser) return;
  event.preventDefault();
  try { await shared.signOut(); location.hash = '#languages'; }
  catch { $('shared-status').textContent = 'Sign-out failed. Try again before switching to the manual editor.'; }
};
$('submit').onclick = async () => {
  if (!sharedUser || submitting || busy || !value(selected)) return;
  const entry = data.entries[selected], text = value(selected), language = lang;
  submitting = true; updateProgress();
  $('draft-status').textContent = 'Submitting for approval…';
  try {
    await shared.submit(language, entry, text);
    $('draft-status').textContent = 'Submitted for admin approval. Your draft is still saved in this browser.';
  } catch (error) {
    $('draft-status').textContent = error.message?.startsWith('Submission limit') ? error.message : 'Submission failed. Your draft is still here. Try again or download the .po file.';
  } finally { submitting = false; updateProgress(); }
};
async function openAdmin() {
  if (!shared || !sharedUser || !['owner','admin'].includes(sharedRole)) return;
  try {
    if (!adminUi) {
      const module = await import('/admin.js');
      adminUi = module.createAdmin(shared, sharedUser, sharedRole, (item, source) => {
        const entry = Object.entries(data.entries).find(([,entry]) => entry.id === item.entry && entry.sourceDigest === item.sourceDigest);
        if (!entry || !source) { $('admin-status').textContent = 'This preview needs a checkout containing the suggestion’s current source.'; return; }
        setDraft(drafts, item.language, entry[0], item.text);
        selectedSubmission = item;
        lang = item.language; preferredKey = entry[0]; save();
        const scene = Object.entries(data.coverage.scenes).find(([,keys]) => keys.includes(entry[0]));
        if (scene) $('scene').value = scene[0];
        location.hash = `#edit/${lang}`;
      }, item => item ? sharedApproved(item) : loadSharedProgress());
    }
    await adminUi.reload();
  } catch (error) { $('admin-status').textContent = error.message; }
}
async function loadAccount() {
  if (!shared || !sharedUser) return;
  profileFields = null; $('account-form').inert = true; $('account-save').disabled = true;
  $('account-status').textContent = 'Loading your account…';
  $('delete-account').disabled = !data.account_deletion_available;
  try {
    const profile = await shared.profile();
    $('account-bio').value = profile.bio || '';
    const [module,response] = await Promise.all([import('/language-picker.js'),fetch('/languages.json')]);
    if (!response.ok) throw Error('The language list could not be loaded.');
    profileFields = module.createLanguageFields(await response.json(),$('native-language'),$('other-languages'),profile);
    $('account-status').textContent = '';
  } catch { $('account-status').textContent = 'Your account could not be loaded. Reopen account settings to try again.'; }
  finally { $('account-form').inert = false; $('account-save').disabled = !profileFields; }
}
$('account-form').onsubmit = async event => {
  event.preventDefault();
  const button = $('account-save'); button.disabled = true;
  try {
    if (!profileFields) throw Error('Reopen account settings to load your profile.');
    await shared.saveProfile({bio:$('account-bio').value,...profileFields.value()});
    $('account-status').textContent = 'Account saved.';
  } catch { $('account-status').textContent = 'Your account could not be saved. Your edits are still here; try again.'; }
  finally { button.disabled = false; }
};
$('delete-account').onclick = async () => {
  if (!confirm('Delete your account and profile? Your translation submissions will become anonymised. This cannot be undone.')) return;
  $('delete-account').disabled = true; $('account-status').textContent = 'Confirming your GitHub identity before deletion…';
  try { const result = await shared.deleteAccount(); location.hash = '#modes'; $('shared-status').textContent = result.pending ? 'Account deletion requested. Your submissions will become anonymised.' : 'Your account was deleted. Your submissions have been anonymised.'; }
  catch (error) { $('account-status').textContent = error.message; }
  finally { $('delete-account').disabled = false; }
};
document.addEventListener('click', event => { if (!$('account-menu').contains(event.target)) $('account-menu').open = false; });
document.addEventListener('keydown', event => { if (event.key === 'Escape' && $('account-menu').open) { $('account-menu').open = false; $('account-menu').querySelector('summary').focus(); } });
async function openLanguageCreator() {
  $('language-create-status').textContent = 'Loading languages…'; $('language-create').disabled = true;
  try {
    const [module,response] = await Promise.all([import('/language-picker.js'),fetch('/languages.json')]);
    if (!response.ok) throw Error('The language list could not be loaded.');
    const names = await response.json();
    languageCreator = module.createLanguageFields(names,$('create-language-field'),document.createElement('div'),{}, {id:'create-language-input',exclude:data.languages.map(code => code.replace('_','-'))});
    const enabled = !sharedUser || ['owner','admin'].includes(sharedRole) || await shared.creationEnabled();
    $('language-create').disabled = !enabled;
    $('language-create').textContent = sharedUser ? 'Create language' : 'Start translating';
    $('language-create-status').textContent = enabled ? '' : 'New languages are currently paused.';
  } catch { $('language-create-status').textContent = 'The language list could not be loaded. Reopen this page to try again.'; }
}
$('language-create-form').onsubmit = async event => {
  event.preventDefault(); if ($('language-create').disabled) return;
  try {
    const chosen = languageCreator.value().nativeLanguage;
    if (!chosen) throw Error('Choose a language.');
    const code = chosen.replace('-','_');
    $('language-create').disabled = true;
    if (sharedUser) { await shared.createLanguage(code); await loadSharedProgress(); if (!data.languages.includes(code)) throw Error('The new language could not be loaded. Reload and try again.'); }
    else { localLanguages = [...new Set([...localLanguages,code])]; addLocalLanguage(code); save(); }
    location.hash = `#edit/${code}`;
  } catch (error) { $('language-create-status').textContent = error.code === 'permission-denied' ? 'New languages are currently paused. Reload to check again.' : error.message; }
  finally { $('language-create').disabled = false; }
};
start();
