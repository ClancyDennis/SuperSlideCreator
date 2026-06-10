(() => {
  // Tag the platform so the stylesheet can pad the topbar past the macOS
  // traffic lights (the Tauri window uses an overlay title bar on macOS).
  (function tagPlatform() {
    const ua = navigator.userAgent;
    if (/Mac OS X/i.test(ua)) document.body.classList.add('platform-macos');
    else if (/Windows/i.test(ua)) document.body.classList.add('platform-windows');
    else document.body.classList.add('platform-linux');
  })();

  const els = {
    ptt: document.getElementById('ptt'),
    stateWord: document.getElementById('stateWord'),
    stateSub: document.getElementById('stateSub'),
    transcript: document.getElementById('transcript'),
    logBody: document.getElementById('logBody'),
    evtCount: document.getElementById('evtCount'),
    evtCountStream: document.getElementById('evtCountStream'),
    clock: document.getElementById('clock'),
    rtt: document.getElementById('rtt'),
    turns: document.getElementById('turns'),
    connPill: document.getElementById('connPill'),
    // Deck
    deckFrame: document.getElementById('deckFrame'),
    deckTitle: document.getElementById('deckTitle'),
    slideCount: document.getElementById('slideCount'),
    buildCount: document.getElementById('buildCount'),
    deckPos: document.getElementById('deckPos'),
    prevSlide: document.getElementById('prevSlide'),
    nextSlide: document.getElementById('nextSlide'),
    deckBuilding: document.getElementById('deckBuilding'),
    deckBuildingText: document.getElementById('deckBuildingText'),
    outline: document.getElementById('outline'),
    // Console drawer
    diagToggle: document.getElementById('diagToggle'),
    diagDrawer: document.getElementById('diagDrawer'),
    diagClose: document.getElementById('diagClose'),
    drawerScrim: document.getElementById('drawerScrim'),
    // Context library drawer
    ctxToggle: document.getElementById('ctxToggle'),
    ctxBadge: document.getElementById('ctxBadge'),
    ctxDrawer: document.getElementById('ctxDrawer'),
    ctxClose: document.getElementById('ctxClose'),
    ctxAddBtn: document.getElementById('ctxAddBtn'),
    ctxFile: document.getElementById('ctxFile'),
    ctxList: document.getElementById('ctxList'),
    ctxNote: document.getElementById('ctxNote'),
    // Image library (within the Context drawer)
    imgAddBtn: document.getElementById('imgAddBtn'),
    imgFile: document.getElementById('imgFile'),
    imgList: document.getElementById('imgList'),
    imgNote: document.getElementById('imgNote'),
    diagRealtime: document.getElementById('diagRealtime'),
    diagSlide: document.getElementById('diagSlide'),
    diagSlideState: document.getElementById('diagSlideState'),
    // Command bar
    cmdForm: document.getElementById('cmdForm'),
    cmdInput: document.getElementById('cmdInput'),
    // Project switcher (topbar button) + picker overlay (landing screen)
    proj: document.getElementById('proj'),
    projBtn: document.getElementById('projBtn'),
    projName: document.getElementById('projName'),
    pickerOverlay: document.getElementById('pickerOverlay'),
    pickerList: document.getElementById('pickerList'),
    pickerNewForm: document.getElementById('pickerNewForm'),
    pickerNewName: document.getElementById('pickerNewName'),
    pickerNote: document.getElementById('pickerNote'),
    // PDF export
    exportPdf: document.getElementById('exportPdf'),
    // Top-bar links that open a secondary view
    openEditor: document.getElementById('openEditor'),
    openDeck: document.getElementById('openDeck'),
  };

  // ——— Open a page in its own app window (desktop) or a browser tab (web) ———
  // In the Tauri shell, target="_blank" does nothing and remote-origin pages
  // can't reach IPC, so we ask the relay to spawn a real app window server-side
  // (POST /api/open-window). On the plain-browser path that route is absent, so
  // we just let the <a target="_blank"> open a tab. Either way it stays inside
  // a predictable surface, not the user's random default browser.
  async function openInAppWindow(e, label, title) {
    const path = e.currentTarget.getAttribute('href');
    e.preventDefault();
    try {
      const res = await fetch('/api/open-window', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({label, path, title}),
      });
      if (res.ok) return;
    } catch (_) { /* fall through to browser tab */ }
    window.open(path, '_blank', 'noopener');
  }
  if (els.openEditor) {
    els.openEditor.addEventListener('click', (e) => openInAppWindow(e, 'editor', 'Slide Editor'));
  }
  if (els.openDeck) {
    els.openDeck.addEventListener('click', (e) => openInAppWindow(e, 'deck', 'Deck'));
  }

  // ——— Clock ———
  function tick() {
    els.clock.textContent = new Date().toLocaleTimeString([], {hour12:false});
  }
  tick(); setInterval(tick, 1000);

  // ——— Console drawer ———
  function setDrawer(open) {
    els.diagDrawer.classList.toggle('is-open', open);
    els.diagDrawer.setAttribute('aria-hidden', open ? 'false' : 'true');
    els.drawerScrim.classList.toggle('is-open', open);
    els.diagToggle.classList.toggle('is-open', open);
  }
  els.diagToggle.addEventListener('click', () => { setCtxDrawer(false); setDrawer(!els.diagDrawer.classList.contains('is-open')); });
  els.diagClose.addEventListener('click', () => setDrawer(false));
  els.drawerScrim.addEventListener('click', () => { setDrawer(false); setCtxDrawer(false); });
  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    if (els.diagDrawer.classList.contains('is-open')) setDrawer(false);
    if (els.ctxDrawer.classList.contains('is-open')) setCtxDrawer(false);
  });

  // ——— Context library drawer ———
  function setCtxDrawer(open) {
    els.ctxDrawer.classList.toggle('is-open', open);
    els.ctxDrawer.setAttribute('aria-hidden', open ? 'false' : 'true');
    els.drawerScrim.classList.toggle('is-open', open);
    els.ctxToggle.classList.toggle('is-open', open);
    if (open) { refreshRefs(); refreshImages(); }
  }
  els.ctxToggle.addEventListener('click', () => { setDrawer(false); setCtxDrawer(!els.ctxDrawer.classList.contains('is-open')); });
  els.ctxClose.addEventListener('click', () => setCtxDrawer(false));

  let refs = [];
  let images = [];
  // Badge = enabled refs + enabled images (everything feeding the next build).
  function updateCtxBadge() {
    const n = refs.filter(r => r.enabled).length + images.filter(i => i.enabled).length;
    els.ctxBadge.textContent = String(n);
  }
  function ctxNote(msg, isErr) {
    if (!msg) { els.ctxNote.hidden = true; return; }
    els.ctxNote.hidden = false;
    els.ctxNote.textContent = msg;
    els.ctxNote.classList.toggle('is-err', !!isErr);
  }
  function imgNote(msg, isErr) {
    if (!msg) { els.imgNote.hidden = true; return; }
    els.imgNote.hidden = false;
    els.imgNote.textContent = msg;
    els.imgNote.classList.toggle('is-err', !!isErr);
  }
  function fmtBytes(n) {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / 1048576).toFixed(1)} MB`;
  }
  function renderRefs() {
    updateCtxBadge();
    if (!refs.length) {
      els.ctxList.innerHTML = '<li class="ctx-empty">no reference files yet</li>';
      return;
    }
    els.ctxList.innerHTML = refs.map(r =>
      `<li class="ctx-item${r.enabled ? '' : ' is-off'}" data-id="${esc(r.id)}">`
      + `<button class="ctx-toggle${r.enabled ? ' on' : ''}" data-act="toggle" title="Include in builds" aria-label="Toggle"></button>`
      + `<div class="ctx-info"><div class="ctx-name" title="${esc(r.name)}">${esc(r.name)}</div>`
      + `<div class="ctx-size">${fmtBytes(r.bytes)}</div></div>`
      + `<button class="ctx-del" data-act="del" title="Remove" aria-label="Remove">×</button></li>`).join('');
    els.ctxList.querySelectorAll('.ctx-item').forEach(li => {
      const id = li.dataset.id;
      li.querySelector('[data-act="toggle"]').addEventListener('click', () => toggleRef(id));
      li.querySelector('[data-act="del"]').addEventListener('click', () => deleteRef(id));
    });
  }
  async function refreshRefs() {
    try {
      const data = await (await fetch('/api/refs')).json();
      refs = data.refs || [];
      renderRefs();
    } catch (_) { ctxNote('could not load reference files', true); }
  }
  async function toggleRef(id) {
    const r = refs.find(x => x.id === id); if (!r) return;
    const next = !r.enabled;
    r.enabled = next; renderRefs(); // optimistic
    try {
      const res = await (await fetch(`/api/refs/${id}`, {
        method: 'PATCH', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({enabled: next}),
      })).json();
      if (!res.ok) throw new Error(res.error || 'toggle failed');
    } catch (e) { r.enabled = !next; renderRefs(); ctxNote(String(e.message || e), true); }
  }
  async function deleteRef(id) {
    try {
      const res = await (await fetch(`/api/refs/${id}`, {method: 'DELETE'})).json();
      if (!res.ok) throw new Error(res.error || 'delete failed');
      refs = refs.filter(r => r.id !== id); renderRefs();
    } catch (e) { ctxNote(String(e.message || e), true); }
  }
  els.ctxAddBtn.addEventListener('click', () => els.ctxFile.click());
  els.ctxFile.addEventListener('change', async (e) => {
    const files = [...(e.target.files || [])];
    els.ctxFile.value = '';
    if (!files.length) return;
    ctxNote(`uploading ${files.length} file${files.length === 1 ? '' : 's'}…`);
    let added = 0, failed = [];
    for (const f of files) {
      try {
        const content = await f.text();
        const res = await (await fetch('/api/refs', {
          method: 'POST', headers: {'Content-Type': 'application/json'},
          body: JSON.stringify({name: f.name, content}),
        })).json();
        if (!res.ok) throw new Error(res.error || 'upload failed');
        added++;
      } catch (err) { failed.push(`${f.name}: ${err.message || err}`); }
    }
    await refreshRefs();
    ctxNote(failed.length ? `added ${added}; failed — ${failed.join('; ')}` : `added ${added} file${added === 1 ? '' : 's'}`, failed.length > 0);
  });

  // ——— Image library (within the Context drawer) ———
  // Thumbnails are fetched lazily by id so the list payload stays small.
  const thumbCache = new Map();
  async function thumbFor(id) {
    if (thumbCache.has(id)) return thumbCache.get(id);
    try {
      const res = await (await fetch(`/api/images/${id}/data`)).json();
      const uri = res.ok ? res.data_uri : '';
      thumbCache.set(id, uri);
      return uri;
    } catch (_) { return ''; }
  }
  // Pin options reflect the current deck size (slideTotal is kept in sync by the
  // deck preview). 0 = "let the agent decide".
  function pinOptions(sel) {
    let html = `<option value="0"${sel === 0 ? ' selected' : ''}>any slide</option>`;
    for (let i = 1; i <= Math.max(slideTotal, sel); i++) {
      html += `<option value="${i}"${sel === i ? ' selected' : ''}>slide ${i}</option>`;
    }
    return html;
  }
  function renderImages() {
    updateCtxBadge();
    if (!images.length) {
      els.imgList.innerHTML = '<li class="ctx-empty">no images yet</li>';
      return;
    }
    els.imgList.innerHTML = images.map(im =>
      `<li class="ctx-img-item${im.enabled ? '' : ' is-off'}" data-id="${esc(im.id)}">`
      + `<div class="ctx-img-top">`
      + `<img class="ctx-img-thumb" data-thumb alt="">`
      + `<div class="ctx-img-head">`
      + `<div class="ctx-item" style="border:0;background:none;padding:0;gap:8px">`
      + `<button class="ctx-toggle${im.enabled ? ' on' : ''}" data-act="toggle" title="Include in builds" aria-label="Toggle"></button>`
      + `<div class="ctx-info"><div class="ctx-name" title="${esc(im.name)}">${esc(im.name)}</div>`
      + `<div class="ctx-size">${fmtBytes(im.bytes)}</div></div>`
      + `<button class="ctx-del" data-act="del" title="Remove" aria-label="Remove">×</button></div>`
      + `</div></div>`
      + `<textarea class="ctx-img-desc" data-desc rows="2" placeholder="describe this image so the agent knows what it shows…">${esc(im.description || '')}</textarea>`
      + `<div class="ctx-img-row"><label class="ctx-img-pin">pin to <select data-pin>${pinOptions(im.pinned_slide || 0)}</select></label>`
      + `<span class="ctx-img-spinner" data-spin></span></div>`
      + `</li>`).join('');
    images.forEach(im => {
      const li = els.imgList.querySelector(`.ctx-img-item[data-id="${cssEsc(im.id)}"]`);
      if (!li) return;
      const thumb = li.querySelector('[data-thumb]');
      thumbFor(im.id).then(uri => { if (uri) thumb.src = uri; });
      li.querySelector('[data-act="toggle"]').addEventListener('click', () => toggleImage(im.id));
      li.querySelector('[data-act="del"]').addEventListener('click', () => deleteImage(im.id));
      const desc = li.querySelector('[data-desc]');
      desc.addEventListener('change', () => patchImage(im.id, {description: desc.value}, li));
      li.querySelector('[data-pin]').addEventListener('change', (e) =>
        patchImage(im.id, {pinned_slide: Number(e.target.value)}, li));
    });
  }
  function cssEsc(s) { return String(s).replace(/["\\]/g, '\\$&'); }
  async function refreshImages() {
    try {
      const data = await (await fetch('/api/images')).json();
      images = data.images || [];
      renderImages();
    } catch (_) { imgNote('could not load images', true); }
  }
  async function toggleImage(id) {
    const im = images.find(x => x.id === id); if (!im) return;
    const next = !im.enabled;
    im.enabled = next; renderImages(); // optimistic
    try {
      const res = await (await fetch(`/api/images/${id}`, {
        method: 'PATCH', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({enabled: next}),
      })).json();
      if (!res.ok) throw new Error(res.error || 'toggle failed');
    } catch (e) { im.enabled = !next; renderImages(); imgNote(String(e.message || e), true); }
  }
  async function patchImage(id, patch, li) {
    const im = images.find(x => x.id === id); if (!im) return;
    Object.assign(im, patch);
    try {
      const res = await (await fetch(`/api/images/${id}`, {
        method: 'PATCH', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify(patch),
      })).json();
      if (!res.ok) throw new Error(res.error || 'save failed');
    } catch (e) { imgNote(String(e.message || e), true); }
  }
  async function deleteImage(id) {
    try {
      const res = await (await fetch(`/api/images/${id}`, {method: 'DELETE'})).json();
      if (!res.ok) throw new Error(res.error || 'delete failed');
      images = images.filter(x => x.id !== id); thumbCache.delete(id); renderImages();
    } catch (e) { imgNote(String(e.message || e), true); }
  }
  function fileToDataUri(file) {
    return new Promise((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => resolve(r.result);
      r.onerror = reject;
      r.readAsDataURL(file);
    });
  }
  els.imgAddBtn.addEventListener('click', () => els.imgFile.click());
  els.imgFile.addEventListener('change', async (e) => {
    const files = [...(e.target.files || [])];
    els.imgFile.value = '';
    if (!files.length) return;
    imgNote(`uploading ${files.length} image${files.length === 1 ? '' : 's'} & describing…`);
    let added = 0, failed = [];
    for (const f of files) {
      try {
        const dataUri = await fileToDataUri(f);
        const res = await (await fetch('/api/images', {
          method: 'POST', headers: {'Content-Type': 'application/json'},
          body: JSON.stringify({name: f.name, data_uri: dataUri}),
        })).json();
        if (!res.ok) throw new Error(res.error || 'upload failed');
        added++;
      } catch (err) { failed.push(`${f.name}: ${err.message || err}`); }
    }
    await refreshImages();
    imgNote(failed.length ? `added ${added}; failed — ${failed.join('; ')}` : `added ${added} image${added === 1 ? '' : 's'}`, failed.length > 0);
  });

  // ——— Audio I/O (carried over from the voice agent, unchanged shape) ———
  let audioCtx, source, stream, workletNode;
  let playCtx = null, playHeadTime = 0;
  let live = false;

  async function prewarmMic() {
    if (stream) return true;
    try {
      stream = await navigator.mediaDevices.getUserMedia({audio: {
        channelCount: 1, echoCancellation: true, noiseSuppression: true
      }});
      log('ok', 'media.permission.granted', '<tag>mic prewarmed</tag>');
      return true;
    } catch (err) {
      log('err', 'media.permission.denied', `<tag>${err.name}: ${err.message}</tag>`);
      return false;
    }
  }

  async function startMic() {
    if (audioCtx) return true;
    try {
      if (!stream) {
        stream = await navigator.mediaDevices.getUserMedia({audio: {
          channelCount: 1, echoCancellation: true, noiseSuppression: true
        }});
      }
      audioCtx = new (window.AudioContext||window.webkitAudioContext)();
      await audioCtx.audioWorklet.addModule('worklet.js');
      source = audioCtx.createMediaStreamSource(stream);
      workletNode = new AudioWorkletNode(audioCtx, 'pcm-downsampler');
      source.connect(workletNode);
      workletNode.port.onmessage = (e) => {
        if (!live || !ws || ws.readyState !== 1) return;
        ws.send(JSON.stringify({type: 'input_audio_buffer.append', audio: arrayBufferToBase64(e.data)}));
      };
      log('ok', 'media.stream.open', `<tag>mic @ ${audioCtx.sampleRate|0} Hz → 24 kHz pcm16</tag>`);
      return true;
    } catch (err) {
      log('err', 'media.stream.error', `<tag>${err.name}: ${err.message}</tag>`);
      return false;
    }
  }

  function arrayBufferToBase64(buf) {
    const bytes = new Uint8Array(buf);
    let s = ''; const CHUNK = 0x8000;
    for (let i = 0; i < bytes.length; i += CHUNK) {
      s += String.fromCharCode.apply(null, bytes.subarray(i, i + CHUNK));
    }
    return btoa(s);
  }
  function base64ToPcm16(b64) {
    const bin = atob(b64);
    const buf = new ArrayBuffer(bin.length), bytes = new Uint8Array(buf);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return new Int16Array(buf);
  }
  function playPcm16Chunk(pcm16) {
    if (!playCtx) {
      playCtx = new (window.AudioContext||window.webkitAudioContext)({sampleRate: 24000});
      playHeadTime = playCtx.currentTime;
    }
    const f32 = new Float32Array(pcm16.length);
    for (let i = 0; i < pcm16.length; i++) f32[i] = pcm16[i] / 0x8000;
    const buf = playCtx.createBuffer(1, f32.length, playCtx.sampleRate);
    buf.copyToChannel(f32, 0);
    const node = playCtx.createBufferSource();
    node.buffer = buf; node.connect(playCtx.destination);
    const start = Math.max(playCtx.currentTime + 0.03, playHeadTime);
    node.start(start);
    playHeadTime = start + buf.duration;
  }
  function stopPlayback() {
    if (!playCtx) return;
    try { playCtx.close(); } catch (_) {}
    playCtx = null; playHeadTime = 0;
  }

  // ——— PTT ———
  const STATE_MOODS = {
    listening:'is-listening', speaking:'is-speaking', thinking:'is-thinking',
    awaiting:'', connecting:'is-thinking', closed:'is-closed',
  };
  function setState(word, sub) {
    els.stateWord.textContent = word;
    if (sub != null) els.stateSub.textContent = sub;
    els.stateWord.classList.remove('is-listening','is-speaking','is-thinking','is-closed');
    const mood = STATE_MOODS[word.toLowerCase()];
    if (mood) els.stateWord.classList.add(mood);
  }

  async function pressPTT() {
    if (live) return;
    if (!(await ensureWs())) return;
    if (!(await startMic())) return;
    if (playCtx) { stopPlayback(); ws.send(JSON.stringify({type:'response.cancel'})); }
    live = true;
    els.ptt.classList.add('pressed','live');
    setState('listening', 'release to send the turn');
  }
  function releasePTT() {
    if (!live) return;
    live = false;
    els.ptt.classList.remove('pressed','live');
    setState('thinking', 'turn sent · waiting for the model');
    if (ws && ws.readyState === 1) {
      ws.send(JSON.stringify({type:'input_audio_buffer.commit'}));
      ws.send(JSON.stringify({type:'response.create'}));
    }
  }
  els.ptt.addEventListener('pointerdown', pressPTT);
  els.ptt.addEventListener('pointerup', releasePTT);
  els.ptt.addEventListener('pointerleave', releasePTT);
  window.addEventListener('keydown', e => {
    if (e.code === 'Space' && !e.repeat &&
        document.activeElement.tagName !== 'INPUT' &&
        document.activeElement.tagName !== 'TEXTAREA') {
      e.preventDefault(); pressPTT();
    }
  });
  window.addEventListener('keyup', e => {
    if (e.code === 'Space' && document.activeElement.tagName !== 'INPUT') {
      e.preventDefault(); releasePTT();
    }
  });

  // ——— Transcript + event log ———
  let evtN = 0;
  function log(level, type, msg) {
    evtN++;
    const ts = new Date().toLocaleTimeString([], {hour12:false});
    const line = document.createElement('div');
    line.className = 'log-line';
    const ico = {ok:'●', err:'✕', info:'◦'}[level] || '·';
    line.innerHTML = `<span class="ts">${ts}</span><span class="ico ${level}">${ico}</span>`
      + `<span class="msg">${type}${msg?` ${msg.startsWith('<tag>')?msg:`<span class="tag">${msg}</span>`}`:''}</span>`;
    els.logBody.appendChild(line);
    els.logBody.scrollTop = els.logBody.scrollHeight;
    if (els.evtCount) els.evtCount.textContent = String(evtN);
    if (els.evtCountStream) els.evtCountStream.textContent = `${evtN} events`;
    while (els.logBody.children.length > 200) els.logBody.removeChild(els.logBody.firstChild);
  }
  function esc(s) {
    return String(s ?? '').replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
  }

  // ——— WebSocket to relay ———
  let ws = null, wsReady = null;
  let pendingUserTurn = null;
  const agentTurnsByItem = new Map();
  let responseStart = 0, turnCount = 0;

  function ensureWs() {
    if (ws && ws.readyState === 1) return Promise.resolve(true);
    if (wsReady) return wsReady;
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    ws = new WebSocket(`${proto}//${location.host}/ws`);
    wsReady = new Promise((resolve) => {
      ws.addEventListener('open', () => {
        log('ok', 'ws.open', `<tag>relay · ${location.host}</tag>`);
        els.connPill.textContent = 'connected';
        els.connPill.classList.remove('err');
        els.ptt.removeAttribute('disabled');
        setState('awaiting', 'hold the button or press space to talk');
        resolve(true);
      });
      ws.addEventListener('close', () => {
        log('err', 'ws.close', '<tag>relay closed</tag>');
        els.connPill.textContent = 'disconnected';
        els.connPill.classList.add('err');
        ws = null; wsReady = null;
        setState('closed', 'relay closed · reload to reconnect');
        resolve(false);
      });
      ws.addEventListener('error', () => log('err', 'ws.error', '<tag>relay error</tag>'));
      ws.addEventListener('message', (ev) => handleServerEvent(ev.data));
    });
    return wsReady;
  }

  function upsertAgentTurn(itemId) {
    const key = itemId || '_anon';
    const existing = agentTurnsByItem.get(key);
    if (existing) return existing;
    const turn = document.createElement('div');
    turn.className = 'turn role-agent';
    const ts = new Date().toLocaleTimeString([], {hour12:false});
    turn.innerHTML = `<div class="turn-meta"><span class="who">Assistant</span><span>${ts}</span></div>`
      + `<div class="turn-text" data-buf=""></div>`;
    els.transcript.appendChild(turn);
    els.transcript.scrollTop = els.transcript.scrollHeight;
    const textEl = turn.querySelector('.turn-text');
    agentTurnsByItem.set(key, textEl);
    return textEl;
  }
  function upsertUserTurn() {
    if (pendingUserTurn) return pendingUserTurn;
    const turn = document.createElement('div');
    turn.className = 'turn role-user';
    const ts = new Date().toLocaleTimeString([], {hour12:false});
    turn.innerHTML = `<div class="turn-meta"><span class="who">You</span><span>${ts}</span></div>`
      + `<div class="turn-text" data-buf=""></div>`;
    els.transcript.appendChild(turn);
    els.transcript.scrollTop = els.transcript.scrollHeight;
    pendingUserTurn = turn.querySelector('.turn-text');
    return pendingUserTurn;
  }
  function renderTurnText(el, text) {
    if (!el) return; el.dataset.buf = text; el.textContent = text;
  }

  function handleServerEvent(raw) {
    let evt; try { evt = JSON.parse(raw); } catch { return; }
    const t = evt.type || '';
    log(t === 'error' ? 'err' : 'ok', t, evt.event_id ? `<tag>${evt.event_id}</tag>` : '');

    // UI-only events from the relay
    if (t === 'ui.deck_update') { renderDeck(evt.deck, evt.html); return; }
    if (t === 'ui.slide_status') { updateSlideStatus(evt); return; }
    if (t === 'ui.build_pending') { showBuilding(evt.instruction); return; }
    if (t === 'ui.build_resolved') { hideBuilding(); return; }

    switch (t) {
      case 'input_audio_buffer.speech_started':
        setState('listening', 'speech detected'); break;
      case 'response.created':
        responseStart = performance.now();
        setState('thinking', 'preparing a reply');
        agentTurnsByItem.clear();
        break;
      case 'response.audio.delta':
        if (evt.delta) { playPcm16Chunk(base64ToPcm16(evt.delta)); setState('speaking', 'hold the button to interrupt'); }
        break;
      case 'response.audio_transcript.delta': {
        const el = upsertAgentTurn(evt.item_id);
        renderTurnText(el, (el.dataset.buf || '') + (evt.delta || ''));
        break;
      }
      case 'conversation.item.input_audio_transcription.delta': {
        const el = upsertUserTurn();
        renderTurnText(el, (el.dataset.buf || '') + (evt.delta || ''));
        break;
      }
      case 'conversation.item.input_audio_transcription.completed':
        if (evt.transcript) renderTurnText(upsertUserTurn(), evt.transcript);
        pendingUserTurn = null;
        break;
      case 'response.done': {
        const rtt = performance.now() - responseStart;
        els.rtt.textContent = `${rtt|0} ms`;
        turnCount += 1; els.turns.textContent = String(turnCount);
        setState('awaiting', 'hold the button or press space to talk');
        agentTurnsByItem.clear();
        break;
      }
      case 'error':
        setState('awaiting', `error: ${evt.error?.code || 'unknown'} · try again`);
        break;
    }
  }

  // ——— Deck rendering ———
  let slideTotal = 0, slidePos = 0, buildCount = 0;

  function renderDeck(deck, htmlDoc) {
    hideBuilding();
    if (deck) {
      els.deckTitle.textContent = deck.title || 'Untitled Deck';
      slideTotal = (deck.slides || []).length;
      els.slideCount.textContent = String(slideTotal);
      renderOutline(deck.slides || []);
    }
    buildCount += 1;
    els.buildCount.textContent = String(buildCount);
    // The deck doc is self-contained; render it in the sandboxed iframe.
    els.deckFrame.srcdoc = htmlDoc || '';
  }

  function renderOutline(slides) {
    if (!slides.length) {
      els.outline.innerHTML = '<li class="outline-empty">slides appear here as you build them</li>';
      return;
    }
    els.outline.innerHTML = slides.map((s, i) =>
      `<li class="outline-item" data-i="${i}"><span class="outline-n">${i+1}</span>`
      + `<span class="outline-t">${esc(s.title || 'Untitled')}</span></li>`).join('');
    [...els.outline.querySelectorAll('.outline-item')].forEach(li => {
      li.addEventListener('click', () => gotoSlide(Number(li.dataset.i)));
    });
    highlightOutline();
  }
  function highlightOutline() {
    [...els.outline.querySelectorAll('.outline-item')].forEach(li => {
      li.classList.toggle('is-current', Number(li.dataset.i) === slidePos);
    });
  }

  function deckNav(dir) {
    if (!els.deckFrame.contentWindow) return;
    els.deckFrame.contentWindow.postMessage({type:'deck-nav', dir}, '*');
  }
  function gotoSlide(i) { deckNav(i); }
  els.prevSlide.addEventListener('click', () => deckNav('prev'));
  els.nextSlide.addEventListener('click', () => deckNav('next'));

  // Position updates flow back from the deck iframe.
  window.addEventListener('message', (e) => {
    const d = e.data || {};
    if (d.type !== 'deck-pos') return;
    slidePos = d.i; slideTotal = d.n;
    els.deckPos.textContent = `${d.i+1} / ${d.n}`;
    els.prevSlide.disabled = d.i <= 0;
    els.nextSlide.disabled = d.i >= d.n - 1;
    highlightOutline();
  });

  function showBuilding(instruction) {
    els.deckBuildingText.textContent = instruction ? `building: ${instruction}` : 'building slides…';
    els.deckBuilding.hidden = false;
    if (els.diagSlideState) els.diagSlideState.textContent = 'building';
  }
  function hideBuilding() {
    els.deckBuilding.hidden = true;
    if (els.diagSlideState) els.diagSlideState.textContent = 'ready';
  }
  function updateSlideStatus(evt) {
    const state = String(evt.state || '').toLowerCase();
    if (els.diagSlideState) els.diagSlideState.textContent = state || '—';
    if (state === 'building') showBuilding(evt.note);
    else if (state === 'ready' || state === 'error') hideBuilding();
  }

  // ——— Typed-command fallback ———
  // Inject the text as a user message + trigger a response. The voice agent
  // treats it exactly like a spoken turn and calls build_slides as needed.
  els.cmdForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    const text = els.cmdInput.value.trim();
    if (!text) return;
    if (!(await ensureWs())) return;
    renderTurnText(upsertUserTurn(), text);
    pendingUserTurn = null;
    ws.send(JSON.stringify({
      type: 'conversation.item.create',
      item: { type: 'message', role: 'user',
        content: [{ type: 'input_text', text }] },
    }));
    ws.send(JSON.stringify({ type: 'response.create' }));
    els.cmdInput.value = '';
    setState('thinking', 'sent · waiting for the model');
  });

  // ——— Boot ———
  window.addEventListener('beforeunload', () => {
    try { if (ws && ws.readyState <= 1) ws.close(1000, 'unload'); } catch {}
  });

  // Placeholder shown in the preview iframe until the first deck is built.
  function seedPlaceholder() {
    els.deckFrame.removeAttribute('src');  // ensure srcdoc isn't shadowed by a stale src
    els.deckFrame.srcdoc = `<!doctype html><meta charset="utf-8"><style>
      html,body{height:100%;margin:0;overflow:hidden;font-family:system-ui,sans-serif;
        background:radial-gradient(120% 120% at 0% 0%,#11151f,#0a0b10 60%);color:#e7e7ea}
      .w{height:100%;display:flex;flex-direction:column;justify-content:center;padding:9vmin 11vmin}
      .k{font:700 13px/1 system-ui;letter-spacing:.22em;text-transform:uppercase;color:#7c8cff;margin-bottom:16px}
      h1{font:800 clamp(30px,6vmin,58px)/1.05 system-ui;letter-spacing:-.02em;margin:0 0 14px}
      p{font:400 clamp(16px,2.4vmin,22px)/1.5 system-ui;color:#aab3c5;max-width:640px}
      </style><div class="w"><div class="k">slide creator</div>
      <h1>No deck yet</h1><p>Press <b>space</b> and say what you want — e.g.
      &ldquo;make five slides about AI in space.&rdquo; Then follow up:
      &ldquo;slide 3 needs a chart&rdquo; or &ldquo;make it all cohesive.&rdquo;</p></div>`;
  }

  // ——— Setup / settings overlay ———
  const setupEls = {
    overlay: document.getElementById('setupOverlay'),
    form: document.getElementById('setupForm'),
    seg: document.getElementById('providerSeg'),
    apiKey: document.getElementById('cfgApiKey'),
    openaiFields: document.getElementById('openaiFields'),
    baseUrl: document.getElementById('cfgBaseUrl'),
    azureFields: document.getElementById('azureFields'),
    azureEndpoint: document.getElementById('cfgAzureEndpoint'),
    realtime: document.getElementById('cfgRealtime'),
    slide: document.getElementById('cfgSlide'),
    image: document.getElementById('cfgImage'),
    voice: document.getElementById('cfgVoice'),
    submit: document.getElementById('setupSubmit'),
    error: document.getElementById('setupError'),
    settingsBtn: document.getElementById('settingsBtn'),
  };
  // Sensible per-provider model defaults; only overwrite the model fields when
  // they're empty or still showing the other provider's defaults.
  const PROVIDER_DEFAULTS = {
    openai: { realtime: 'gpt-realtime-2', slide: 'gpt-5.5', image: 'gpt-image-2' },
    azure:  { realtime: 'gpt-realtime-2', slide: 'gpt-5.5', image: 'gpt-image-2' },
  };
  let currentProvider = 'openai';

  function applyProviderUI(provider) {
    currentProvider = provider;
    [...setupEls.seg.querySelectorAll('.seg-btn')].forEach(b =>
      b.classList.toggle('active', b.dataset.provider === provider));
    setupEls.azureFields.hidden = provider !== 'azure';
    setupEls.openaiFields.hidden = provider !== 'openai';
    setupEls.apiKey.placeholder = provider === 'azure' ? 'Azure resource key' : 'sk-…';
  }
  setupEls.seg.addEventListener('click', (e) => {
    const btn = e.target.closest('.seg-btn');
    if (btn) applyProviderUI(btn.dataset.provider);
  });

  function showSetup() { setupEls.overlay.hidden = false; }
  function hideSetup() { setupEls.overlay.hidden = true; }

  async function loadConfigIntoForm() {
    let data = {};
    try { data = await (await fetch('/api/config')).json(); } catch (_) {}
    applyProviderUI(data.provider || 'openai');
    setupEls.baseUrl.value = data.openai_base_url || '';
    setupEls.azureEndpoint.value = data.azure_endpoint || '';
    const d = PROVIDER_DEFAULTS[data.provider || 'openai'];
    setupEls.realtime.value = data.realtime_model || d.realtime;
    setupEls.slide.value = data.slide_model || d.slide;
    setupEls.image.value = data.image_model || d.image;
    // Select the saved voice; if it isn't a known option (older config or a
    // value the API added), inject it so it still displays and round-trips.
    const v = data.voice || 'alloy';
    if (![...setupEls.voice.options].some(o => o.value === v)) {
      const opt = document.createElement('option');
      opt.value = v; opt.textContent = v.charAt(0).toUpperCase() + v.slice(1);
      setupEls.voice.appendChild(opt);
    }
    setupEls.voice.value = v;
    // Key is never returned; leave blank with a hint if one is saved.
    setupEls.apiKey.value = '';
    setupEls.apiKey.placeholder = data.api_key_set
      ? '•••••• (saved · retype to change)'
      : (data.provider === 'azure' ? 'Azure resource key' : 'sk-…');
    // Mirror models into the diagnostics panel.
    if (els.diagRealtime) els.diagRealtime.textContent = setupEls.realtime.value;
    if (els.diagSlide) els.diagSlide.textContent = setupEls.slide.value;
    return data;
  }

  setupEls.settingsBtn.addEventListener('click', async () => {
    setupEls.error.textContent = '';
    await loadConfigIntoForm();
    showSetup();
  });

  setupEls.form.addEventListener('submit', async (e) => {
    e.preventDefault();
    setupEls.error.textContent = '';
    const payload = {
      provider: currentProvider,
      openai_base_url: setupEls.baseUrl.value.trim(),
      azure_endpoint: setupEls.azureEndpoint.value.trim(),
      realtime_model: setupEls.realtime.value.trim(),
      slide_model: setupEls.slide.value.trim(),
      image_model: setupEls.image.value.trim(),
      voice: setupEls.voice.value.trim() || 'alloy',
    };
    const key = setupEls.apiKey.value.trim();
    if (key) payload.api_key = key;            // blank = keep saved key
    if (currentProvider === 'azure' && !payload.azure_endpoint) {
      setupEls.error.textContent = 'Azure endpoint is required.';
      return;
    }
    setupEls.submit.disabled = true;
    setupEls.submit.textContent = 'Saving…';
    try {
      const resp = await fetch('/api/config', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      });
      const data = await resp.json();
      if (!resp.ok || !data.ok) throw new Error(data.error || `save failed (${resp.status})`);
      if (!data.configured) {
        setupEls.error.textContent = 'Still missing required fields (did you enter the API key?).';
        return;
      }
      hideSetup();
      if (els.diagRealtime) els.diagRealtime.textContent = payload.realtime_model;
      if (els.diagSlide) els.diagSlide.textContent = payload.slide_model;
      if (sessionStarted) {
        // Mid-session settings change: reconnect so the new config (voice,
        // model, provider, key) takes effect on a fresh socket.
        startSession({ reconnect: true });
      } else {
        // First-run setup just completed → go to the project picker.
        showPicker();
      }
    } catch (err) {
      setupEls.error.textContent = String(err.message || err);
    } finally {
      setupEls.submit.disabled = false;
      setupEls.submit.textContent = 'Save & continue';
    }
  });

  // ——— Projects ———
  // Each project is an isolated deck (its own slides, theme, refs, images). The
  // picker overlay is the landing screen and the switch/manage surface — all
  // inline (NB: Tauri's webview has no window.prompt/confirm/alert, so we never
  // use them). Opening a project starts/reconnects the session for that deck.
  let projects = [];
  let activeProject = null;
  let sessionStarted = false;  // has a deck session been opened this run?

  function pickerNote(msg, isErr) {
    if (!msg) { els.pickerNote.hidden = true; return; }
    els.pickerNote.hidden = false;
    els.pickerNote.textContent = msg;
    els.pickerNote.classList.toggle('is-err', !!isErr);
  }
  function showPicker() { renderPicker(); els.pickerOverlay.hidden = false; pickerNote(''); }
  function hidePicker() { els.pickerOverlay.hidden = true; }

  els.projBtn.addEventListener('click', () => showPicker());
  // Esc closes the picker only once a deck is open (on first run you must pick).
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && !els.pickerOverlay.hidden && sessionStarted) hidePicker();
  });

  function renderPicker() {
    if (!projects.length) {
      els.pickerList.innerHTML = '<li class="picker-empty">No projects yet — create one below.</li>';
      return;
    }
    els.pickerList.innerHTML = projects.map(p => {
      const active = p.id === activeProject;
      return `<li class="picker-item${active ? ' is-active' : ''}" data-id="${esc(p.id)}">`
        + `<button class="picker-open" data-act="open" title="Open this project">`
        + `<span class="picker-dot">${active ? '●' : '○'}</span>`
        + `<span class="picker-name" data-name="${esc(p.id)}">${esc(p.name)}</span></button>`
        + `<button class="picker-icon" data-act="rename" title="Rename">✎</button>`
        + `<button class="picker-icon picker-icon-danger" data-act="del" title="Delete"`
        + `${projects.length <= 1 ? ' disabled' : ''}>×</button></li>`;
    }).join('');
    els.pickerList.querySelectorAll('.picker-item').forEach(li => {
      const id = li.dataset.id;
      li.querySelector('[data-act="open"]').addEventListener('click', () => openProject(id));
      li.querySelector('[data-act="rename"]').addEventListener('click', (e) => { e.stopPropagation(); beginRename(id); });
      const del = li.querySelector('[data-act="del"]');
      if (del && !del.disabled) del.addEventListener('click', (e) => { e.stopPropagation(); deleteProject(id); });
    });
  }

  // Inline rename: swap the name span for an input in place.
  function beginRename(id) {
    const span = els.pickerList.querySelector(`.picker-name[data-name="${CSS.escape(id)}"]`);
    if (!span) return;
    const cur = projects.find(p => p.id === id);
    const input = document.createElement('input');
    input.className = 'picker-rename-input';
    input.value = cur ? cur.name : '';
    span.replaceWith(input);
    input.focus(); input.select();
    const commit = async () => {
      const name = input.value.trim();
      if (name && cur && name !== cur.name) {
        try {
          const res = await (await fetch(`/api/projects/${id}`, {
            method:'PATCH', headers:{'Content-Type':'application/json'},
            body: JSON.stringify({name}),
          })).json();
          if (!res.ok) throw new Error(res.error || 'rename failed');
        } catch (e) { pickerNote(String(e.message || e), true); }
      }
      await refreshProjects(); renderPicker();
    };
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') { e.preventDefault(); input.blur(); }
      else if (e.key === 'Escape') { e.preventDefault(); renderPicker(); }
    });
    input.addEventListener('blur', commit, { once: true });
  }

  async function deleteProject(id) {
    const p = projects.find(x => x.id === id);
    const wasActive = id === activeProject;
    pickerNote(`Deleting “${p ? p.name : id}”…`);
    try {
      const res = await (await fetch(`/api/projects/${id}`, {method:'DELETE'})).json();
      if (!res.ok) throw new Error(res.error || 'delete failed');
      await refreshProjects();   // server picks a survivor as active
      renderPicker();
      pickerNote('');
      // If we deleted the deck we were working in, reload the now-active one
      // into the (background) view so it's correct when the picker closes.
      if (wasActive && sessionStarted) {
        els.transcript.innerHTML = '';
        await loadActiveDeck();
        refreshRefs(); refreshImages();
        startSession({ reconnect: true });
      }
    } catch (e) { pickerNote(String(e.message || e), true); }
  }

  async function refreshProjects() {
    try {
      const data = await (await fetch('/api/projects')).json();
      projects = data.projects || [];
      activeProject = data.active || (projects[0] && projects[0].id) || null;
      const active = projects.find(p => p.id === activeProject);
      els.projName.textContent = active ? active.name : '—';
    } catch (_) {}
  }

  // Open a project: activate it server-side, then start (or reconnect) the
  // session and load that deck. This is also how we leave the landing screen.
  async function openProject(id) {
    try {
      if (id && id !== activeProject) {
        const res = await (await fetch(`/api/projects/${id}/activate`, {method:'POST'})).json();
        if (!res.ok) throw new Error(res.error || 'switch failed');
      }
    } catch (e) { pickerNote(String(e.message || e), true); return; }
    await refreshProjects();
    hidePicker();
    els.transcript.innerHTML = '';
    await loadActiveDeck();
    refreshRefs(); refreshImages();
    startSession({ reconnect: sessionStarted });
    sessionStarted = true;
  }

  // Load the active project's saved deck into the preview + outline.
  async function loadActiveDeck() {
    try {
      const d = await (await fetch('/api/deck')).json();
      els.deckTitle.textContent = d.title || 'No deck yet';
      slideTotal = (d.slides || []).length;
      els.slideCount.textContent = String(slideTotal);
      renderOutline(d.slides || []);
      if ((d.slides || []).length) {
        // srcdoc wins over src when both are set, so clear the placeholder's
        // srcdoc before pointing at the rendered deck.
        els.deckFrame.removeAttribute('srcdoc');
        els.deckFrame.src = '/slides/deck.html?ts=' + Date.now();
      } else {
        seedPlaceholder();
      }
    } catch (_) { seedPlaceholder(); }
  }

  els.pickerNewForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    const name = els.pickerNewName.value.trim() || 'Untitled Project';
    pickerNote('Creating…');
    try {
      const res = await (await fetch('/api/projects', {
        method:'POST', headers:{'Content-Type':'application/json'},
        body: JSON.stringify({name}),
      })).json();
      if (!res.ok) throw new Error(res.error || 'create failed');
      els.pickerNewName.value = '';
      await openProject(res.id);   // creating opens it straight away
    } catch (e) { pickerNote(String(e.message || e), true); }
  });

  // ——— Boot ———
  window.addEventListener('beforeunload', () => {
    try { if (ws && ws.readyState <= 1) ws.close(1000, 'unload'); } catch {}
  });

  function startSession(opts = {}) {
    // On a settings change, drop any live socket so ensureWs() opens a fresh
    // one (carrying the new voice/model in its session.update). Stop playback
    // and reset turn state so the new session starts clean.
    if (opts.reconnect && ws) {
      try { ws.close(1000, 'reconfigure'); } catch {}
      ws = null; wsReady = null;
      stopPlayback();
      pendingUserTurn = null;
      agentTurnsByItem.clear();
    }
    setState('connecting', 'opening the voice line…');
    prewarmMic();
    ensureWs();
  }

  // ——— Export to PDF ———
  // The Tauri/WKWebView ignores window.print(), so we open the deck's print
  // layout in the user's DEFAULT BROWSER (via the relay), where it auto-opens
  // the print dialog and offers "Save as PDF". No external tools / no Chrome dep.
  let exporting = false;
  async function exportToPdf() {
    if (exporting) return;
    exporting = true;
    const btn = els.exportPdf;
    const label = btn.querySelector('span');
    const prev = label ? label.textContent : '';
    if (label) label.textContent = 'Opening…';
    btn.disabled = true;
    try {
      const res = await (await fetch('/api/export/open', {method:'POST'})).json();
      if (!res.ok) throw new Error(res.error || 'export failed');
      log('ok', 'pdf.export', '<tag>opened in browser · use Save as PDF</tag>');
    } catch (e) {
      log('err', 'pdf.export', `<tag>${e.message || e}</tag>`);
    } finally {
      if (label) label.textContent = prev;
      btn.disabled = false;
      exporting = false;
    }
  }
  els.exportPdf.addEventListener('click', exportToPdf);

  (async function boot() {
    seedPlaceholder();
    await refreshProjects();        // active project name in the topbar
    refreshRefs(); refreshImages(); // populate the Context badge/lists
    const data = await loadConfigIntoForm();
    if (data && data.configured) {
      // Land on the project picker — the user chooses a deck to open, which
      // starts the session. (No auto-connect, so decks never get mixed up.)
      setState('awaiting', 'choose a project to begin');
      showPicker();
    } else {
      // First run: no key yet. Setup first; on save we show the picker.
      setState('awaiting', 'add your API key to begin');
      showSetup();
    }
  })();
})();
