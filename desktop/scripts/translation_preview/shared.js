// Loaded only after choosing GitHub sign-in; the manual editor has no Firebase dependency.
const version = '13.1.0';
let service;
function split(text) {
  const pieces = [];
  for (let index = 0; index < text.length;) {
    let end = Math.min(text.length, index + 150000);
    if (end < text.length && /[\uD800-\uDBFF]/.test(text[end - 1])) end--;
    pieces.push(text.slice(index,end)); index = end;
  }
  return pieces;
}
// The project service handles one request at a time per instance, so busy replies are retried
export async function projectAction(action, body) {
  for (let attempt = 0; ; attempt++) {
    const response = await fetch('/api/project-action', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({action,...body})});
    if (response.ok) return action === 'export' ? response.blob() : response.json();
    if (![429,500,502,503,504].includes(response.status) || attempt >= 3) {
      let result; try { result = await response.json(); } catch {}
      throw Error(result?.error || 'The project service could not handle this request. Try again shortly.');
    }
    await new Promise(resolve => setTimeout(resolve, 1500 * (attempt + 1)));
  }
}
export function connect(onUser, {emulators = false} = {}) {
  service ||= (async () => {
    const [app, auth, store, response] = await Promise.all([
      import(`https://www.gstatic.com/firebasejs/${version}/firebase-app.js`),
      import(`https://www.gstatic.com/firebasejs/${version}/firebase-auth.js`),
      import(`https://www.gstatic.com/firebasejs/${version}/firebase-firestore.js`),
      fetch('/firebase-config.json'),
    ]);
    if (!response.ok) throw Error('Shared project configuration is unavailable. You can still download a .po file.');
    const instance = app.initializeApp(emulators ? {projectId:'demo-telescope', apiKey:'demo-key', authDomain:location.host} : await response.json());
    const session = auth.getAuth(instance), database = store.getFirestore(instance);
    if (emulators) { auth.connectAuthEmulator(session, 'http://127.0.0.1:9099', {disableWarnings:true}); store.connectFirestoreEmulator(database, '127.0.0.1', 8085); }
    await auth.setPersistence(session, auth.browserSessionPersistence);
    let currentRole, currentUsername;
    async function usernameFor(user) {
      const reference = store.doc(database,'identities',user.uid);
      const saved = await store.getDoc(reference);
      if (saved.exists()) return saved.data().username;
      const response = await fetch('/api/identity',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({idToken:await user.getIdToken()})});
      if (!response.ok) throw Error('Your GitHub username could not be verified. Sign out and try again.');
      for (let attempt = 0; attempt < 30; attempt++) {
        await new Promise(resolve => setTimeout(resolve,1000));
        const identity = await store.getDoc(reference);
        if (identity.exists()) return identity.data().username;
      }
      throw Error('Your GitHub username is still being verified. Sign out and try again shortly.');
    }
    auth.onAuthStateChanged(session, async user => {
      try {
        const role = user ? await store.getDoc(store.doc(database, 'roles', user.uid)) : null;
        currentRole = role?.data()?.role || null;
        currentUsername = user ? await usernameFor(user) : null;
        onUser(user, currentRole, currentUsername);
      } catch (error) { currentRole = null; currentUsername = null; onUser(user, null, null, error.message); }
    });
    async function createLimited(kind, target, reference, value, also = []) {
      const limit = kind === 'submissions' ? 100 : 3, interval = kind === 'submissions' ? 3600000 : 86400000;
      if (['owner','admin'].includes(currentRole)) {
        const batch = store.writeBatch(database);
        for (const [item, content] of [[reference, value], ...also]) batch.set(item, content);
        await batch.commit(); return;
      }
      const usage = store.doc(database,'limits',session.currentUser.uid,'actions',kind);
      await store.runTransaction(database, async transaction => {
        const [existing, snapshot] = await Promise.all([transaction.get(reference),transaction.get(usage)]);
        if (existing.exists()) return;
        const previous = snapshot.data(), reset = !previous || Date.now() - previous.window.toMillis() >= interval;
        const count = reset ? 1 : previous.count + 1;
        if (count > limit) throw Error(kind === 'submissions' ? 'Submission limit reached. Try again in an hour.' : 'You can add three languages per day. Try again tomorrow.');
        transaction.set(usage,{window:reset ? store.serverTimestamp() : previous.window,count,lastTarget:target});
        transaction.set(reference,value);
        for (const [item, content] of also) transaction.set(item, content);
      });
    }
    async function readChunks(collection, version, parts) {
      if (!Number.isInteger(parts) || parts < 1 || parts > 200) throw Error('The shared project version is invalid.');
      const chunks = await Promise.all(Array.from({length:parts}, (_,index) => store.getDoc(store.doc(database, collection, version, 'chunks', String(index)))));
      if (chunks.some(chunk => !chunk.exists())) return null;
      return JSON.parse(chunks.map(chunk => chunk.data().content).join(''));
    }
    async function saveProjection(version, projection) {
      const pieces = split(JSON.stringify(projection));
      await Promise.all(pieces.map((content, index) => store.setDoc(store.doc(database, 'projections', version, 'chunks', String(index)), {content})));
      const pointer = store.doc(database, 'workspace', 'current');
      await store.runTransaction(database, async transaction => {
        const current = (await transaction.get(pointer)).data();
        if (current?.version === version && !current.projectionParts) transaction.update(pointer, {projectionParts:pieces.length});
      });
    }
    return {
      signIn: () => emulators ? auth.signInWithRedirect(session, new auth.GithubAuthProvider()) : auth.signInWithPopup(session, new auth.GithubAuthProvider()),
      signOut: () => auth.signOut(session),
      get username() { return currentUsername; },
      async profile() { return (await store.getDoc(store.doc(database, 'profiles', session.currentUser.uid))).data() || {}; },
      // The private profile keeps the account's own copy; people/USERNAME is what other contributors see
      async saveProfile(profile) {
        if (!currentUsername) throw Error('Your GitHub username has not been verified yet. Sign out and try again.');
        const batch = store.writeBatch(database);
        batch.set(store.doc(database, 'profiles', session.currentUser.uid), profile);
        batch.set(store.doc(database, 'people', currentUsername), profile);
        await batch.commit();
      },
      async person(username) { return (await store.getDoc(store.doc(database, 'people', username))).data() || null; },
      async suggestions(language) { return (await store.getDocs(store.query(store.collection(database, 'suggestions'), store.where('language','==',language)))).docs.map(item => ({id:item.id,...item.data()})); },
      async suggestionAuthor(id) { return (await store.getDoc(store.doc(database, 'submissions', id))).data()?.authorUid || null; },
      async deleteAccount() {
        await auth.reauthenticateWithPopup(session.currentUser, new auth.GithubAuthProvider());
        const idToken = await session.currentUser.getIdToken(true);
        const response = await fetch('/api/delete-account', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({idToken})});
        if (!response.ok) { let result; try { result = await response.json(); } catch {} throw Error(result?.error || 'Account deletion failed. Try again.'); }
        const result = await response.json();
        await auth.signOut(session);
        return result;
      },
      async roleList() { return Promise.all((await store.getDocs(store.collection(database, 'roles'))).docs.map(async item => ({uid:item.id,...item.data(),username:(await store.getDoc(store.doc(database,'identities',item.id))).data()?.username || 'Contributor'}))); },
      async grantAdmin(uid) { await store.setDoc(store.doc(database, 'roles', uid), {role:'admin',grantedBy:session.currentUser.uid}); },
      async revokeAdmin(uid) { await store.deleteDoc(store.doc(database, 'roles', uid)); },
      async pending() { return (await store.getDocs(store.query(store.collection(database, 'suggestions'), store.where('status','==','pending')))).docs.map(item => ({id:item.id,...item.data()})); },
      async submissions() { return (await store.getDocs(store.collection(database, 'submissions'))).docs.map(item => ({id:item.id,...item.data()})); },
      async approvals() {
        return (await store.getDocs(store.collection(database, 'approvals'))).docs.map(item => {
          const data = item.data();
          return {...data,id:item.id,approvedAt:data.approvedAt?.toDate().toISOString() || new Date().toISOString()};
        });
      },
      async languages() { return (await store.getDocs(store.collection(database, 'languages'))).docs.map(item => item.id); },
      async loadProject() {
        const pointer = (await store.getDoc(store.doc(database, 'workspace', 'current'))).data();
        if (!pointer) return {project:null,version:null};
        const project = await readChunks('versions', pointer.version, pointer.parts);
        if (!project) throw Error('The shared project version is incomplete. Reload in a moment.');
        if (pointer.privateParts && ['owner','admin'].includes(currentRole)) project.submissions = (await readChunks('privateVersions', pointer.version, pointer.privateParts))?.submissions || [];
        return {project,version:pointer.version};
      },
      // Everything the editor and review list need, without downloading the whole project
      async loadProgress() {
        const [pointer, approvals, languages] = await Promise.all([store.getDoc(store.doc(database, 'workspace', 'current')).then(item => item.data()),this.approvals(),this.languages()]);
        if (!pointer) return {projection:null,approvals,languages,version:null};
        let projection = pointer.projectionParts ? await readChunks('projections', pointer.version, pointer.projectionParts) : null;
        if (!projection) {
          const {project} = await this.loadProject();
          projection = await projectAction('status', {platform:project});
          if (['owner','admin'].includes(currentRole)) await saveProjection(pointer.version, projection).catch(() => {});
        }
        return {projection,approvals,languages,version:pointer.version};
      },
      async creationEnabled() { return (await store.getDoc(store.doc(database, 'settings', 'project'))).data()?.allowLanguageCreation !== false; },
      async setCreationEnabled(enabled) { await store.setDoc(store.doc(database, 'settings', 'project'), {allowLanguageCreation:enabled}); },
      async createLanguage(language) {
        if (!currentUsername) throw Error('Your GitHub username has not been verified yet. Sign out and try again.');
        const reference = store.doc(database, 'languages', language);
        if ((await store.getDoc(reference)).exists()) return;
        await createLimited('languages',language,reference,{createdBy:'github:'+currentUsername,createdAt:store.serverTimestamp()});
      },
      async saveProject(project, expectedVersion, projection) {
        const {submissions, ...published} = project;
        const version = crypto.randomUUID(), pieces = split(JSON.stringify(published));
        const privatePieces = submissions?.length ? split(JSON.stringify({submissions})) : [];
        if (pieces.length > 200 || privatePieces.length > 200) throw Error('This project is too large for the prototype.');
        const projectionPieces = split(JSON.stringify(projection));
        await Promise.all([
          ...pieces.map((content, index) => store.setDoc(store.doc(database, 'versions', version, 'chunks', String(index)), {content})),
          ...privatePieces.map((content, index) => store.setDoc(store.doc(database, 'privateVersions', version, 'chunks', String(index)), {content})),
          ...projectionPieces.map((content, index) => store.setDoc(store.doc(database, 'projections', version, 'chunks', String(index)), {content})),
        ]);
        const pointer = store.doc(database, 'workspace', 'current');
        await store.runTransaction(database, async transaction => {
          const current = (await transaction.get(pointer)).data();
          if ((current?.version || null) !== expectedVersion) throw Error('Another administrator imported or restored the project meanwhile. Reload before applying your change.');
          transaction.set(pointer, {version,parts:pieces.length,privateParts:privatePieces.length,projectionParts:projectionPieces.length,updatedBy:'github:'+currentUsername,updatedAt:store.serverTimestamp()});
        });
        return version;
      },
      async approve({language, entry, sourceDigest, text, author, submission = ''}) {
        if (!currentUsername) throw Error('Your GitHub username has not been verified yet. Sign out and try again.');
        const reviewer = 'github:'+currentUsername, batch = store.writeBatch(database);
        batch.set(store.doc(database, 'approvals', `${language}-${entry}`), {language,entry,sourceDigest,text,reviewer,author,submission,approvedAt:store.serverTimestamp()});
        if (submission) for (const name of ['submissions','suggestions']) batch.update(store.doc(database, name, submission), {status:'approved',reviewedBy:reviewer,reviewedAt:store.serverTimestamp()});
        await batch.commit();
        return reviewer;
      },
      async dismiss(submission) {
        if (!currentUsername) throw Error('Your GitHub username has not been verified yet. Sign out and try again.');
        const batch = store.writeBatch(database);
        for (const name of ['submissions','suggestions']) batch.update(store.doc(database, name, submission), {status:'dismissed',reviewedBy:'github:'+currentUsername,reviewedAt:store.serverTimestamp()});
        await batch.commit();
      },
      async submit(language, entry, text) {
        if (!session.currentUser) throw Error('Sign in with GitHub before submitting.');
        if (!currentUsername) throw Error('Your GitHub username has not been verified yet. Sign out and try again.');
        const bytes = new TextEncoder().encode(JSON.stringify([session.currentUser.uid, language, entry.id, entry.sourceDigest, text]));
        const hash = await crypto.subtle.digest('SHA-256', bytes);
        const id = Array.from(new Uint8Array(hash), byte => byte.toString(16).padStart(2, '0')).join('');
        const reference = store.doc(database, 'submissions', id);
        if ((await store.getDoc(reference)).exists()) return id;
        // The private submission holds the account id for quotas and erasure; the suggestion is what everyone sees
        await createLimited('submissions',id,reference, {language, entry:entry.id, sourceDigest:entry.sourceDigest,
          text, authorUid:session.currentUser.uid, authorUsername:currentUsername, status:'pending', createdAt:store.serverTimestamp()},
          [[store.doc(database, 'suggestions', id), {language, entry:entry.id, sourceDigest:entry.sourceDigest, text, author:'github:'+currentUsername, status:'pending', createdAt:store.serverTimestamp()}]]);
        return reference.id;
      },
    };
  })().catch(error => { service = null; throw error; });
  return service;
}
