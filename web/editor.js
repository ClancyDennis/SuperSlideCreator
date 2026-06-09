/* Slide Creator — manual editor controller.
 *
 * Loads the current deck from /api/deck (which renders a same-origin WYSIWYG
 * document) into an iframe, then drives editing entirely through postMessage —
 * the same contract the voice app's preview uses. The iframe runtime (EDITOR_JS
 * in deck.py) owns the DOM; this page owns the chrome (rail, inspector, save).
 *
 * Save flow: ask the iframe to serialize its slides → POST /api/deck. The
 * server merges by slide id onto deck.json (the shared source of truth) and
 * writes deck.html, so the next AI build sees these manual edits.
 */
(() => {
  const els = {
    frame: document.getElementById('editorFrame'),
    deckTitle: document.getElementById('deckTitle'),
    outline: document.getElementById('outline'),
    saveBtn: document.getElementById('saveBtn'),
    saveState: document.getElementById('saveState'),
    saveSub: document.getElementById('saveSub'),
    // inspector
    inspKind: document.getElementById('inspKind'),
    inspEmpty: document.getElementById('inspEmpty'),
    inspText: document.getElementById('inspText'),
    inspImage: document.getElementById('inspImage'),
    inspMeta: document.getElementById('inspMeta'),
    fldColor: document.getElementById('fldColor'),
    fldBg: document.getElementById('fldBg'),
    fldSize: document.getElementById('fldSize'),
    fldSizeVal: document.getElementById('fldSizeVal'),
    fldWeight: document.getElementById('fldWeight'),
    fldAlign: document.getElementById('fldAlign'),
    clearStyle: document.getElementById('clearStyle'),
    // image
    inspImgMeta: document.getElementById('inspImgMeta'),
    uploadImgBtn: document.getElementById('uploadImgBtn'),
    uploadImg: document.getElementById('uploadImg'),
    imgPrompt: document.getElementById('imgPrompt'),
    imgSize: document.getElementById('imgSize'),
    regenImgBtn: document.getElementById('regenImgBtn'),
    imgNote: document.getElementById('imgNote'),
  };

  let slides = [];        // [{id, title}]
  let curSlide = 0;
  let curSel = null;      // last selection info from the iframe
  let dirty = false;

  // ——— Save state pill ———
  function setSave(state, sub) {
    const map = {dirty:'is-dirty', saving:'is-saving', saved:'is-saved', err:'is-err', loaded:''};
    els.saveState.className = 'state-pill ' + (map[state] || '');
    els.saveState.textContent =
      {dirty:'unsaved', saving:'saving…', saved:'saved', err:'save failed', loaded:'loaded'}[state] || state;
    if (sub != null) els.saveSub.textContent = sub;
  }
  function markDirty() {
    if (!dirty) { dirty = true; setSave('dirty', 'unsaved changes — press Save'); }
  }

  // ——— Iframe messaging ———
  function toFrame(msg) {
    const w = els.frame.contentWindow;
    if (w) w.postMessage(msg, '*');
  }

  // ——— Load deck ———
  async function load() {
    let data;
    try {
      data = await (await fetch('/api/deck')).json();
    } catch (e) {
      setSave('err', 'could not load the deck'); return;
    }
    slides = data.slides || [];
    document.title = `${data.title || 'Untitled'} — Editor`;
    els.deckTitle.textContent = data.title || 'editor';
    renderOutline();
    // srcdoc keeps the editor doc same-origin with this page (so it can talk
    // back), while still being an isolated document.
    els.frame.srcdoc = data.editor_html || '';
  }

  function renderOutline() {
    if (!slides.length) {
      els.outline.innerHTML =
        '<li class="outline-empty">no slides — build a deck in the voice app first</li>';
      return;
    }
    els.outline.innerHTML = slides.map((s, i) =>
      `<li class="outline-item${i === curSlide ? ' is-current' : ''}" data-i="${i}">`
      + `<span class="outline-n">${i + 1}</span>`
      + `<span class="outline-t">${esc(s.title || 'Untitled')}</span></li>`).join('');
    [...els.outline.querySelectorAll('.outline-item')].forEach(li => {
      li.addEventListener('click', () => gotoSlide(Number(li.dataset.i)));
    });
  }
  function gotoSlide(i) {
    curSlide = i;
    toFrame({type: 'ed-show', i});
    [...els.outline.querySelectorAll('.outline-item')].forEach(li =>
      li.classList.toggle('is-current', Number(li.dataset.i) === i));
  }
  function esc(s) {
    return String(s ?? '').replace(/[&<>"]/g, c =>
      ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
  }

  // ——— Selection → inspector ———
  function showSelection(info) {
    curSel = info;
    if (!info) {
      els.inspKind.textContent = 'Nothing selected';
      els.inspEmpty.hidden = false;
      els.inspText.hidden = true;
      els.inspImage.hidden = true;
      return;
    }
    els.inspEmpty.hidden = true;
    if (info.kind === 'image') {
      els.inspKind.textContent = 'Image';
      els.inspText.hidden = true;
      els.inspImage.hidden = false;
      els.inspImgMeta.textContent = info.imgId ? `image · ${info.imgId}` : 'image';
      return;
    }
    // text
    els.inspKind.textContent = (info.cls.split(/\s+/)[0] || info.tag) || 'Text';
    els.inspImage.hidden = true;
    els.inspText.hidden = false;
    els.inspMeta.textContent = `<${info.tag}> ${info.cls ? '.' + info.cls.split(/\s+/).join('.') : ''}`;
    if (info.color) els.fldColor.value = info.color;
    if (info.background) els.fldBg.value = info.background;
    els.fldSize.value = info.fontSize || 24;
    els.fldSizeVal.textContent = `${info.fontSize || '—'}px`;
    // Normalize weight (e.g. "normal"→400, "bold"→700) to nearest option.
    const w = ({normal:'400', bold:'700'}[info.fontWeight] || String(info.fontWeight || '400'));
    els.fldWeight.value = [...els.fldWeight.options].some(o => o.value === w) ? w : '400';
    [...els.fldAlign.querySelectorAll('button')].forEach(b =>
      b.classList.toggle('is-active', b.dataset.val === info.textAlign));
  }

  // ——— Style controls → iframe ———
  function applyStyle(prop, value) {
    toFrame({type: 'ed-style', prop, value});
    markDirty();
  }
  els.fldColor.addEventListener('input', e => applyStyle('color', e.target.value));
  els.fldBg.addEventListener('input', e => applyStyle('backgroundColor', e.target.value));
  els.fldSize.addEventListener('input', e => {
    els.fldSizeVal.textContent = `${e.target.value}px`;
    applyStyle('fontSize', `${e.target.value}px`);
  });
  els.fldWeight.addEventListener('change', e => applyStyle('fontWeight', e.target.value));
  els.fldAlign.querySelectorAll('button').forEach(b => {
    b.addEventListener('click', () => {
      [...els.fldAlign.querySelectorAll('button')].forEach(x => x.classList.remove('is-active'));
      b.classList.add('is-active');
      applyStyle('textAlign', b.dataset.val);
    });
  });
  els.clearStyle.addEventListener('click', () => {
    toFrame({type: 'ed-clear-style'});
    markDirty();
  });

  // ——— Image: upload ———
  els.uploadImgBtn.addEventListener('click', () => els.uploadImg.click());
  els.uploadImg.addEventListener('change', async e => {
    const file = e.target.files && e.target.files[0];
    if (!file) return;
    const dataUri = await fileToDataUri(file);
    setImgNote('uploading…');
    try {
      const res = await (await fetch('/api/editor/image', {
        method: 'POST', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({data_uri: dataUri}),
      })).json();
      if (!res.ok) throw new Error(res.error || 'upload failed');
      toFrame({type: 'ed-set-image', dataUri: res.data_uri, imgId: res.image_id});
      setImgNote('replaced — press Save to keep it');
      markDirty();
    } catch (err) {
      setImgNote(String(err.message || err), true);
    }
    els.uploadImg.value = '';
  });
  function fileToDataUri(file) {
    return new Promise((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => resolve(r.result);
      r.onerror = reject;
      r.readAsDataURL(file);
    });
  }

  // ——— Image: AI regen ———
  els.regenImgBtn.addEventListener('click', async () => {
    const prompt = els.imgPrompt.value.trim();
    if (!prompt) { setImgNote('describe the image first', true); return; }
    els.regenImgBtn.disabled = true;
    setImgNote('generating… (this can take ~30–60s)');
    try {
      const res = await (await fetch('/api/editor/image', {
        method: 'POST', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({prompt, size: els.imgSize.value}),
      })).json();
      if (!res.ok) throw new Error(res.error || 'generation failed');
      toFrame({type: 'ed-set-image', dataUri: res.data_uri, imgId: res.image_id});
      setImgNote('generated — press Save to keep it');
      markDirty();
    } catch (err) {
      setImgNote(String(err.message || err), true);
    }
    els.regenImgBtn.disabled = false;
  });
  function setImgNote(msg, isErr) {
    els.imgNote.hidden = false;
    els.imgNote.textContent = msg;
    els.imgNote.classList.toggle('is-err', !!isErr);
  }

  // ——— Save ———
  let savePending = null;
  els.saveBtn.addEventListener('click', () => requestSave());
  function requestSave() {
    setSave('saving', 'serializing slides…');
    savePending = true;
    toFrame({type: 'ed-serialize'});
  }
  async function doSave(serialized) {
    savePending = null;
    const payload = {
      title: els.deckTitle.textContent,
      slides: serialized.map(s => ({
        id: slides[s.i] ? slides[s.i].id : '',
        html: s.html,
      })).filter(s => s.id),
    };
    try {
      const res = await (await fetch('/api/deck', {
        method: 'POST', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify(payload),
      })).json();
      if (!res.ok) throw new Error(res.error || 'save failed');
      dirty = false;
      setSave('saved', `saved ${res.updated} slide${res.updated === 1 ? '' : 's'}`);
    } catch (err) {
      setSave('err', String(err.message || err));
    }
  }

  // ——— Messages from the iframe runtime ———
  window.addEventListener('message', e => {
    const d = e.data || {};
    if (d.type === 'ed-ready') { /* iframe up; nothing required */ }
    else if (d.type === 'ed-select') {
      if (typeof d.i === 'number' && d.i !== curSlide) gotoSlide(d.i);
      showSelection(d.info);
    }
    else if (d.type === 'ed-deck') { if (savePending) doSave(d.slides || []); }
  });

  // Text edits happen inside the iframe; treat any keypress there as dirtying.
  // (We can't read the iframe's keystrokes cross-document reliably, so we mark
  // dirty whenever a text element is selected and the user returns focus — and
  // unconditionally on save we re-serialize, so edits are always captured.)
  els.frame.addEventListener('load', () => {
    try {
      els.frame.contentWindow.addEventListener('input', markDirty, true);
      els.frame.contentWindow.addEventListener('keydown', e => {
        if ((e.metaKey || e.ctrlKey) && e.key === 's') { e.preventDefault(); requestSave(); }
      });
    } catch (_) { /* same-origin expected; ignore if blocked */ }
  });

  // Cmd/Ctrl+S from the chrome too.
  window.addEventListener('keydown', e => {
    if ((e.metaKey || e.ctrlKey) && e.key === 's') { e.preventDefault(); requestSave(); }
  });
  window.addEventListener('beforeunload', e => {
    if (dirty) { e.preventDefault(); e.returnValue = ''; }
  });

  load();
})();
