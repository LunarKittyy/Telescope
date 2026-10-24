import {projectAction} from '/shared.js';
const $ = id => document.getElementById(id);
const text = (tag, content) => { const element = document.createElement(tag); element.textContent = content; return element; };
export function createAdmin(service, user, role, preview, changed = async () => {}) {
  let progress, staged, active = false;
  const status = message => { $('admin-status').textContent = message; };
  // The whole project is only downloaded for imports, exports and backups; reviews are separate documents
  async function current(action, extra = {}) {
    const [{project, version}, approvals, languages] = await Promise.all([service.loadProject(), service.approvals(), service.languages()]);
    if (!project && !['initialise','restore'].includes(action)) throw Error('Import a repository snapshot first.');
    const result = await projectAction(action, {platform:project, approvals, languages, ...extra});
    return {...result, version};
  }
  async function publish({platform, projection, version}) {
    await service.saveProject(platform, version, projection);
    staged = null; $('project-apply').hidden = true;
    await refresh();
    await changed();
  }
  async function run(work) {
    if (active) return;
    active = true; $('admin-view').setAttribute('aria-busy','true');
    for (const button of $('admin-view').querySelectorAll('.admin-tools button, #admin-reload, #conflict-list button')) button.disabled = true;
    try { await work(); } catch (error) { status(error.message); }
    finally { active = false; $('admin-view').setAttribute('aria-busy','false'); for (const button of $('admin-view').querySelectorAll('.admin-tools button, #admin-reload, #conflict-list button')) button.disabled = button.dataset.unavailable === 'true'; }
  }
  function download(content, name) {
    const url = URL.createObjectURL(content); const link = document.createElement('a'); link.href = url; link.download = name; link.click(); setTimeout(() => URL.revokeObjectURL(url),1000);
  }
  async function stage(incoming) {
    const pointer = await service.loadProject();
    if (incoming.kind === 'platform' && pointer.project) throw Error('Restore a project backup into an empty project. Use a repository snapshot to merge updates here.');
    const action = incoming.kind === 'platform' ? 'restore' : pointer.project ? 'merge' : 'initialise';
    staged = await current(action, {incoming});
    const before = progress?.projection?.english || {}, after = staged.projection.english;
    const added = Object.keys(after).filter(key => !before[key]).length, archived = Object.keys(before).filter(key => !after[key]).length;
    status(`Import ready. ${added} new texts, ${archived} removed texts archived, ${staged.projection.conflicts.length} conflicts to resolve. Nothing has been applied yet.`);
    $('project-apply').hidden = false;
  }
  // Reviews touch one row at a time so several can run without waiting for each other
  async function review(row, work, done) {
    const buttons = row.querySelectorAll('button');
    for (const button of buttons) button.disabled = true;
    row.setAttribute('aria-busy','true');
    try { await work(); row.remove(); status(done); if (!$('pending-list').childElementCount) $('pending-list').append(text('p','No suggestions are waiting for approval.')); }
    catch (error) { status(error.code === 'permission-denied' ? 'This suggestion was already reviewed or your admin access changed. Reload the project.' : error.message); for (const button of buttons) button.disabled = button.dataset.unavailable === 'true'; }
    finally { row.setAttribute('aria-busy','false'); }
  }
  function pendingRows(pending) {
    const projection = progress.projection;
    $('pending-list').replaceChildren();
    if (!pending.length) $('pending-list').append(text('p','No suggestions are waiting for approval.'));
    for (const item of pending.sort((a,b) => (a.createdAt?.toMillis?.() || 0) - (b.createdAt?.toMillis?.() || 0))) {
      const english = projection?.english[item.entry], stale = english !== undefined && projection.sources[item.entry] !== item.sourceDigest;
      const row = document.createElement('article'); row.className = 'pending-row';
      row.append(text('small',`${item.language} · ${item.authorUid === 'anonymous' ? 'Anonymous' : item.authorUsername || 'Contributor'}`),text('h4',english ?? 'Source not in the current project'),text('p',item.text));
      if (stale) row.append(text('small','The source text changed after this was suggested.'));
      const actions = document.createElement('div'); actions.className = 'row-actions';
      const inspect = text('button','Try in preview'); inspect.onclick = () => preview(item,english); actions.append(inspect);
      const approve = text('button','Approve'); approve.className = 'primary'; approve.dataset.unavailable = String(english === undefined || stale);
      approve.disabled = approve.dataset.unavailable === 'true';
      approve.onclick = () => review(row, async () => {
        const approval = {language:item.language,entry:item.entry,sourceDigest:item.sourceDigest,text:item.text,submission:item.id,
          author:item.authorUid === 'anonymous' ? 'anonymous' : 'github:'+(item.authorUsername || 'Contributor')};
        await changed({...approval,reviewer:await service.approve(approval)});
      }, 'Translation approved. It will be included in the language export.');
      actions.append(approve);
      const reject = text('button','Dismiss'); reject.onclick = () => review(row, () => service.dismiss(item.id), 'Suggestion dismissed.'); actions.append(reject);
      if (role === 'owner' && item.authorUid !== 'anonymous' && item.authorUid !== user.uid) {
        const grant = text('button','Make contributor an admin'); grant.onclick = () => run(async () => { await service.grantAdmin(item.authorUid); await roles(); status('Admin access granted.'); }); actions.append(grant);
      }
      row.append(actions); $('pending-list').append(row);
    }
  }
  function conflictRows() {
    const conflicts = progress.projection?.conflicts || [];
    $('merge-conflicts').hidden = !conflicts.length;
    $('conflict-list').replaceChildren();
    for (const [index, conflict] of conflicts.entries()) {
      const row = document.createElement('article'); row.className = 'conflict-row';
      row.append(text('small', conflict.language),text('h4',progress.projection.english[conflict.entry]),text('p',`Platform: ${conflict.platform.forms.join('\n')}\nGit: ${conflict.repository.forms.join('\n')}`));
      const actions = document.createElement('div'); actions.className = 'row-actions';
      for (const [choice,label] of [['platform','Keep platform version'],['repository','Use Git version']]) {
        const button = text('button',label); button.onclick = () => run(async () => { await publish(await current('resolve',{index,choice})); status('Conflict resolved.'); }); actions.append(button);
      }
      row.append(actions); $('conflict-list').append(row);
    }
  }
  async function roles() {
    $('admin-roles').hidden = role !== 'owner';
    if (role !== 'owner') return;
    $('role-list').replaceChildren();
    for (const member of await service.roleList()) {
      const row = document.createElement('div'); row.append(text('span',`${member.uid === user.uid ? 'You' : member.username || 'Contributor'} · ${member.role}`));
      if (member.role === 'admin') { const revoke = text('button','Revoke admin access'); revoke.onclick = () => run(async () => { await service.revokeAdmin(member.uid); await roles(); status('Admin access revoked.'); }); row.append(revoke); }
      $('role-list').append(row);
    }
  }
  async function refresh() {
    const [loaded, pending, creation] = await Promise.all([service.loadProgress(), service.pending(), service.creationEnabled(), roles()]);
    progress = loaded;
    $('allow-language-creation').checked = creation;
    $('admin-language').replaceChildren();
    const names = new Intl.DisplayNames(['en'],{type:'language'});
    for (const language of [...new Set([...Object.keys(progress.projection?.translations || {}),...progress.languages])].sort()) $('admin-language').add(new Option(names.of(language.replace('_','-')),language));
    conflictRows();
    pendingRows(pending);
  }
  async function reload() { await run(async () => { status('Loading the project…'); await refresh(); status(progress.projection ? '' : 'Import a repository snapshot or use this checkout to start the shared project.'); }); }
  $('admin-reload').onclick = reload;
  $('save-project-settings').onclick = () => run(async () => { await service.setCreationEnabled($('allow-language-creation').checked); status('Project settings saved.'); });
  $('project-checkout').onclick = () => run(async () => { const response = await fetch('/api/project-snapshot'); if (!response.ok) throw Error('The checkout snapshot could not be loaded.'); await stage(await response.json()); });
  $('project-import').onclick = () => run(async () => { const file = $('project-file').files[0]; if (!file || file.size > 16000000) throw Error('Choose a repository snapshot smaller than 16 MB.'); await stage(JSON.parse(await file.text())); });
  $('project-apply').onclick = () => run(async () => { if (!staged) return; await publish(staged); status('Import applied. Platform changes have been preserved.'); });
  $('approved-export').onclick = () => run(async () => { const language = $('admin-language').value; const {project} = await service.loadProject(); if (!project) throw Error('Import a repository snapshot first.'); const [approvals, languages] = await Promise.all([service.approvals(), service.languages()]); download(await projectAction('export',{platform:project,approvals,languages,language}),`telescope-${language}.zip`); status('Language exported with its approval records.'); });
  $('shared-po-import').onclick = () => run(async () => { const file = $('shared-po-file').files[0], author = $('shared-po-author').value.trim(); if (!file || !author || file.size > 4000000) throw Error('Choose a .po file smaller than 4 MB and enter a GitHub username or Anonymous.'); await publish(await current('import-po',{language:$('admin-language').value,content:await file.text(),author})); status('Drafts imported.'); });
  $('project-backup').onclick = () => run(async () => { const [folded, submissions] = await Promise.all([current('fold'), service.submissions()]); const backup = {...folded.platform,submissions:[...new Map([...(folded.platform.submissions || []),...submissions].map(item => [item.id,item])).values()]}; download(new Blob([JSON.stringify(backup)],{type:'application/json'}),'telescope-translation-project.json'); status('Project backup downloaded.'); });
  return {reload};
}
