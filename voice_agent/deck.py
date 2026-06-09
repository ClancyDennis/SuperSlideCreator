"""Deck data model + deterministic standalone-HTML renderer.

A deck is *semantic content* (slides written with a small, stable vocabulary
of class names) plus a single ``theme_css`` blob that styles those classes.
Keeping look-and-feel entirely in ``theme_css`` is what makes "make it all
cohesive" cheap: we regenerate one stylesheet and every slide restyles.

``render_deck_html`` assembles a fully self-contained .html file — base
"deck mechanics" CSS (one slide visible at a time, keyboard/click nav) plus
the agent's theme, plus a tiny nav script that also talks to the parent window
via postMessage so the app chrome can drive navigation and show a counter.

The string we render is BOTH what we save to ``slides/deck.html`` and what we
push to the browser (as an iframe ``srcdoc``) — preview equals export.
"""
from __future__ import annotations

import html as _html
import re
from dataclasses import dataclass, field
from typing import Any

# Matches an <img …> tag carrying a data-img="<id>" placeholder, capturing the id.
_IMG_PLACEHOLDER_RE = re.compile(r'data-img="([^"]+)"')

# Trailing-integer extractor, e.g. "s7" -> 7, "img12" -> 12, "x" -> 0.
_ID_SUFFIX_RE = re.compile(r"(\d+)$")


def _max_numeric_suffix(ids: "Any") -> int:
    """Highest trailing integer across an iterable of ids (0 if none)."""
    best = 0
    for i in ids:
        m = _ID_SUFFIX_RE.search(str(i))
        if m:
            best = max(best, int(m.group(1)))
    return best


# The recommended semantic vocabulary the slide agent writes against. Listed
# here so the renderer's base CSS and the agent's system prompt stay in sync.
SEMANTIC_CLASSES = (
    "kicker", "slide-title", "slide-subtitle", "slide-lead",
    "bullets", "columns", "col", "big-number", "label",
    "card", "quote", "footer", "slide-image",
)


@dataclass
class Slide:
    title: str = ""
    html: str = ""  # inner HTML written against the semantic vocabulary
    # Stable id so granular edits (edit/delete/move) target a slide by identity,
    # not by position — positions shift when several ops run in one turn.
    id: str = ""

    def to_dict(self) -> dict[str, Any]:
        return {"id": self.id, "title": self.title, "html": self.html}


@dataclass
class Deck:
    title: str = "Untitled Deck"
    theme_css: str = ""
    slides: list[Slide] = field(default_factory=list)
    # Generated images, keyed by stable image id (img1, img2, …). Each value is
    # {"data_uri": "data:image/png;base64,…", "prompt": "<what was asked>"}.
    # Slides reference an image by id via `<img data-img="img1">`; the renderer
    # injects the real src at render time. Storing the (heavy) base64 here — not
    # in slide html or the model's view of the deck — keeps the model context
    # small while the *rendered* html stays fully self-contained.
    images: dict[str, dict[str, str]] = field(default_factory=dict)
    _seq: int = 0  # monotonic counter for minting slide ids
    _img_seq: int = 0  # monotonic counter for minting image ids

    def to_dict(self) -> dict[str, Any]:
        return {
            "title": self.title,
            "theme_css": self.theme_css,
            "slides": [s.to_dict() for s in self.slides],
            # Only metadata — never the base64 — so the model knows which images
            # exist (to place/reuse them) without paying for the bytes.
            "images": [
                {"id": iid, "prompt": img.get("prompt", "")}
                for iid, img in self.images.items()
            ],
        }

    def to_full_dict(self) -> dict[str, Any]:
        """Complete, round-trippable snapshot — INCLUDING image base64 and the
        id counters — for persisting to ``slides/deck.json``. This is the shared
        source of truth: both the AI agent and the manual editor read/write it,
        so manual edits survive a later AI build (the agent reloads this first).
        Distinct from ``to_dict`` (which omits the heavy base64 to keep the
        model's context small)."""
        return {
            "title": self.title,
            "theme_css": self.theme_css,
            "slides": [
                {"id": s.id, "title": s.title, "html": s.html} for s in self.slides
            ],
            "images": self.images,
            "_seq": self._seq,
            "_img_seq": self._img_seq,
        }

    @classmethod
    def from_full_dict(cls, data: dict[str, Any]) -> "Deck":
        """Rebuild a Deck from a ``to_full_dict`` snapshot. Tolerant of missing
        keys so a hand-edited or partial deck.json still loads."""
        deck = cls(
            title=str(data.get("title") or "Untitled Deck"),
            theme_css=str(data.get("theme_css") or ""),
            images=dict(data.get("images") or {}),
        )
        for s in data.get("slides") or []:
            if not isinstance(s, dict):
                continue
            deck.slides.append(Slide(
                title=str(s.get("title", "")),
                html=str(s.get("html", "")),
                id=str(s.get("id", "")),
            ))
        # Restore counters; if absent (e.g. a hand-written file), derive a safe
        # high-water mark from existing ids so new ids never collide.
        deck._seq = int(data.get("_seq") or 0)
        deck._img_seq = int(data.get("_img_seq") or 0)
        deck._seq = max(deck._seq, _max_numeric_suffix(s.id for s in deck.slides))
        deck._img_seq = max(deck._img_seq, _max_numeric_suffix(deck.images.keys()))
        deck.ensure_ids()
        return deck

    def new_id(self) -> str:
        self._seq += 1
        return f"s{self._seq}"

    def new_image_id(self) -> str:
        self._img_seq += 1
        return f"img{self._img_seq}"

    def ensure_ids(self) -> None:
        """Mint ids for any slide missing one (e.g. from a full rewrite)."""
        for s in self.slides:
            if not s.id:
                s.id = self.new_id()

    def index_of(self, slide_id: str) -> int:
        for i, s in enumerate(self.slides):
            if s.id == slide_id:
                return i
        return -1


# ——— Fixed "deck mechanics" — never agent-controlled. Theme CSS is appended
# after this, so a theme can still override colours/fonts/spacing freely. ———
BASE_CSS = """
*{box-sizing:border-box;margin:0;padding:0}
html,body{height:100%;width:100%;overflow:hidden}
body{font-family:system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;
  background:#0b0c10;color:#e7e7ea}
#deck{position:relative;height:100vh;width:100vw}
.slide{position:absolute;inset:0;flex-direction:column;
  justify-content:center;padding:7vmin 10vmin;overflow:hidden}
/* One slide at a time. !important so a theme that styles `.slide` (e.g.
   display:flex) can't accidentally reveal every slide at once — the theme
   still controls layout of the *active* slide freely. */
.slide:not(.active){display:none!important}
.slide.active{animation:slidein .35s ease}
@keyframes slidein{from{opacity:0;transform:translateY(14px)}to{opacity:1;transform:none}}
.slide-inner{width:100%;max-width:1100px;margin:0 auto;
  transform-origin:center center;transition:transform .2s ease}
/* Sensible default for generated images; themes can override freely. The
   max-height keeps a hero image from eating the whole slide when text sits
   beside or under it; object-fit:contain avoids distortion. */
.slide-image{max-width:100%;max-height:48vh;border-radius:14px;display:block;
  object-fit:contain}
.columns .slide-image{max-height:62vh}
.slide-image.broken{display:none}
.deck-nav{position:fixed;bottom:16px;right:20px;display:flex;gap:8px;
  align-items:center;z-index:20;opacity:.55;transition:opacity .2s}
.deck-nav:hover{opacity:1}
.deck-nav button{cursor:pointer;border:1px solid currentColor;background:transparent;
  color:inherit;border-radius:7px;padding:3px 11px;font:13px system-ui}
.slide-counter{position:fixed;bottom:18px;left:20px;font:12px/1 system-ui;
  letter-spacing:.08em;opacity:.45;z-index:20}
""".strip()


# Nav script: arrow keys + click-to-advance, plus postMessage bridge so the
# app chrome can drive navigation (deck-nav) and mirror position (deck-pos).
NAV_JS = """
(function(){
  var slides=[].slice.call(document.querySelectorAll('.slide'));
  var i=0;
  // Auto-fit safety net: if a slide's content is taller/wider than the usable
  // area (slide minus its padding), scale .slide-inner down so nothing spills
  // off-canvas or gets clipped. Runs after layout, after images load, and on
  // resize. The model aims to fit on its own; this guarantees it.
  function fit(slide){
    var inner=slide.querySelector('.slide-inner'); if(!inner)return;
    inner.style.transform='none';
    var cs=getComputedStyle(slide);
    var availW=slide.clientWidth-parseFloat(cs.paddingLeft)-parseFloat(cs.paddingRight);
    var availH=slide.clientHeight-parseFloat(cs.paddingTop)-parseFloat(cs.paddingBottom);
    var needW=inner.scrollWidth, needH=inner.scrollHeight;
    var scale=Math.min(1, availW/needW, availH/needH);
    inner.style.transform = scale<0.999 ? 'scale('+scale.toFixed(3)+')' : 'none';
  }
  function fitActive(){ var s=slides[i]; if(s) requestAnimationFrame(function(){fit(s)}); }
  // Re-fit when generated images finish decoding (they change content height).
  slides.forEach(function(s){
    [].forEach.call(s.querySelectorAll('img'),function(im){
      if(!im.complete) im.addEventListener('load',fitActive);
      im.addEventListener('error',fitActive);
    });
  });
  window.addEventListener('resize',fitActive);
  function show(n){
    i=Math.max(0,Math.min(slides.length-1,n));
    slides.forEach(function(s,k){s.classList.toggle('active',k===i)});
    var c=document.querySelector('.slide-counter');
    if(c)c.textContent=(i+1)+' / '+slides.length;
    fitActive();
    try{parent.postMessage({type:'deck-pos',i:i,n:slides.length},'*')}catch(e){}
  }
  function next(){show(i+1)} function prev(){show(i-1)}
  document.addEventListener('keydown',function(e){
    if(e.key==='ArrowRight'||e.key==='PageDown'){e.preventDefault();next()}
    else if(e.key==='ArrowLeft'||e.key==='PageUp'){e.preventDefault();prev()}
  });
  document.addEventListener('click',function(e){
    if(e.target.closest('.deck-nav'))return;
    if(window.getSelection&&String(window.getSelection()))return;
    (e.clientX/window.innerWidth>0.5?next:prev)();
  });
  window.addEventListener('message',function(e){
    var d=e.data||{}; if(d.type!=='deck-nav')return;
    if(d.dir==='next')next(); else if(d.dir==='prev')prev();
    else if(typeof d.dir==='number')show(d.dir);
  });
  var nb=document.querySelector('.deck-nav');
  if(nb){var p=nb.querySelector('[data-prev]'),n=nb.querySelector('[data-next]');
    if(p)p.onclick=prev; if(n)n.onclick=next;}
  show(0);
})();
""".strip()


# ——— Editor mode ——————————————————————————————————————————————————————————
# A second runtime, used by the /editor page. The slide markup + BASE_CSS +
# theme are IDENTICAL to the presentable render (so editing is true WYSIWYG),
# but the nav script is swapped for an editing script and a thin overlay of
# editor-only CSS is appended. The editor iframe is served SAME-ORIGIN (no
# sandbox) so the parent can't reach into it via the DOM directly — we still
# drive everything through postMessage, which keeps the contract identical to
# the preview iframe and means the same renderer output works in both.

EDITOR_CSS = """
/* Editor-only chrome. Appended after the theme so it always wins. Nothing here
   ships in the presentable export — render_deck_html omits it. */
html,body{overflow:auto!important}
.slide{cursor:default}
.slide-inner [data-ed-hover]{outline:1.5px dashed rgba(110,168,255,.6);outline-offset:3px}
.slide-inner [data-ed-sel]{outline:2px solid #6ea8ff!important;outline-offset:3px;
  border-radius:3px}
.slide-inner [contenteditable=true]{cursor:text;outline:2px solid #2ecc71!important;
  outline-offset:3px}
.slide-image{cursor:pointer}
.deck-nav,.slide-counter{display:none!important}
/* In editor mode show the *selected* slide; never auto-scale (a transform makes
   the text caret land in the wrong place while typing). */
""".strip()


# Editor runtime. Talks to the parent /editor page entirely via postMessage:
#   parent → iframe:  {type:'ed-show', i}            switch visible slide
#                     {type:'ed-style', prop, value} set inline style on selection
#                     {type:'ed-clear-style'}        strip the selection's inline styles
#                     {type:'ed-set-image', dataUri, imgId}  swap selected image
#                     {type:'ed-serialize'}          request the edited deck back
#   iframe → parent:  {type:'ed-ready', n}
#                     {type:'ed-select', info|null}  selection changed (styles, kind)
#                     {type:'ed-deck', slides:[{i,html}]}  serialized response
EDITOR_JS = r"""
(function(){
  var slides=[].slice.call(document.querySelectorAll('.slide'));
  var i=0, sel=null;
  function show(n){
    i=Math.max(0,Math.min(slides.length-1,n));
    slides.forEach(function(s,k){s.classList.toggle('active',k===i)});
  }
  // What counts as a directly-selectable element: anything inside slide-inner.
  function selectable(el){
    if(!el) return null;
    var inner=el.closest('.slide-inner'); if(!inner) return null;
    if(el===inner){ // clicked the bare canvas — select nothing
      return null;
    }
    return el;
  }
  function clearSel(){
    if(sel){ sel.removeAttribute('data-ed-sel');
      if(sel.getAttribute('contenteditable')==='true'){
        sel.removeAttribute('contenteditable');
      }
    }
    sel=null;
  }
  function describe(el){
    if(!el) return null;
    var cs=getComputedStyle(el);
    var isImg=el.classList.contains('slide-image')||el.tagName==='IMG';
    // px font size as an integer for the panel slider/number box.
    var fs=parseFloat(cs.fontSize)||0;
    return {
      kind: isImg ? 'image' : 'text',
      tag: el.tagName.toLowerCase(),
      cls: el.getAttribute('class')||'',
      imgId: isImg ? (el.getAttribute('data-img')||'') : '',
      color: rgbToHex(cs.color),
      background: rgbToHex(cs.backgroundColor),
      fontSize: Math.round(fs),
      fontWeight: cs.fontWeight,
      textAlign: cs.textAlign,
      // Echo back any styles the user has *explicitly* set inline so the panel
      // can show "set" vs "inherited".
      inline: el.getAttribute('style')||''
    };
  }
  function rgbToHex(c){
    if(!c) return '';
    var m=c.match(/rgba?\(([^)]+)\)/); if(!m) return c;
    var p=m[1].split(',').map(function(x){return parseFloat(x)});
    if(p.length>=4 && p[3]===0) return ''; // transparent
    function h(n){return ('0'+Math.round(n).toString(16)).slice(-2)}
    return '#'+h(p[0])+h(p[1])+h(p[2]);
  }
  function postSel(){
    try{parent.postMessage({type:'ed-select',info:describe(sel),i:i},'*')}catch(e){}
  }

  document.addEventListener('click',function(e){
    var el=selectable(e.target);
    // Clicking the already-editing element: let the caret move normally.
    if(sel && sel.getAttribute('contenteditable')==='true' && sel.contains(e.target)) return;
    if(el===sel){ return; }
    clearSel();
    if(el){ sel=el; sel.setAttribute('data-ed-sel','1'); }
    postSel();
  });

  // Double-click a text element → edit it in place.
  document.addEventListener('dblclick',function(e){
    var el=selectable(e.target);
    if(!el) return;
    if(el.classList.contains('slide-image')||el.tagName==='IMG') return;
    clearSel(); sel=el; sel.setAttribute('data-ed-sel','1');
    sel.setAttribute('contenteditable','true');
    sel.focus();
    postSel();
  });
  document.addEventListener('blur',function(e){
    if(e.target && e.target.getAttribute &&
       e.target.getAttribute('contenteditable')==='true'){
      e.target.removeAttribute('contenteditable');
    }
  },true);

  // Hover affordance (skip while editing).
  document.addEventListener('mouseover',function(e){
    var el=selectable(e.target); if(!el) return;
    if(el.getAttribute('contenteditable')==='true') return;
    el.setAttribute('data-ed-hover','1');
  });
  document.addEventListener('mouseout',function(e){
    if(e.target&&e.target.removeAttribute) e.target.removeAttribute('data-ed-hover');
  });

  // Strip editor-only artifacts and return clean inner HTML for one slide.
  function cleanSlide(slide){
    var inner=slide.querySelector('.slide-inner'); if(!inner) return '';
    var c=inner.cloneNode(true);
    [].forEach.call(c.querySelectorAll('[data-ed-sel],[data-ed-hover],[contenteditable]'),
      function(n){ n.removeAttribute('data-ed-sel'); n.removeAttribute('data-ed-hover');
        n.removeAttribute('contenteditable'); });
    // Images: drop the injected src (and broken markers) so the saved html keeps
    // only the data-img placeholder — render_deck_html re-injects the real src.
    [].forEach.call(c.querySelectorAll('img'),function(im){
      if(im.getAttribute('data-img')){ im.removeAttribute('src'); }
      im.classList.remove('broken'); im.removeAttribute('data-broken');
    });
    // Drop empty inline style="" attributes for tidiness.
    [].forEach.call(c.querySelectorAll('[style=""]'),function(n){n.removeAttribute('style')});
    return c.innerHTML;
  }

  window.addEventListener('message',function(e){
    var d=e.data||{};
    if(d.type==='ed-show'){ clearSel(); show(d.i); postSel(); }
    else if(d.type==='ed-style'){
      if(sel){ sel.style[d.prop]=d.value; postSel(); }
    }
    else if(d.type==='ed-clear-style'){
      if(sel){ sel.removeAttribute('style'); postSel(); }
    }
    else if(d.type==='ed-set-image'){
      if(sel && (sel.classList.contains('slide-image')||sel.tagName==='IMG')){
        if(d.imgId) sel.setAttribute('data-img',d.imgId);
        if(d.dataUri) sel.setAttribute('src',d.dataUri);
        sel.classList.remove('broken'); sel.removeAttribute('data-broken');
        postSel();
      }
    }
    else if(d.type==='ed-serialize'){
      var out=slides.map(function(s,k){return {i:k, html:cleanSlide(s)}});
      try{parent.postMessage({type:'ed-deck',slides:out},'*')}catch(err){}
    }
  });

  show(0);
  try{parent.postMessage({type:'ed-ready',n:slides.length},'*')}catch(e){}
})();
""".strip()


def render_deck_editor_html(deck: Deck) -> str:
    """Assemble the WYSIWYG editor document. Same slide markup + BASE_CSS + theme
    as ``render_deck_html`` (so editing matches the final look exactly), but the
    editor runtime + overlay CSS replace the presentation nav."""
    theme = deck.theme_css.strip() or DEFAULT_THEME_CSS
    title = _html.escape(deck.title or "Untitled Deck")

    if deck.slides:
        sections = "\n".join(
            f'<section class="slide{" active" if i == 0 else ""}" '
            f'data-i="{i}" data-id="{_html.escape(s.id, quote=True)}">'
            f'<div class="slide-inner">{_inject_images(s.html, deck.images)}</div></section>'
            for i, s in enumerate(deck.slides)
        )
    else:
        sections = (
            '<section class="slide active"><div class="slide-inner">'
            '<h1 class="slide-title">No slides yet</h1></div></section>'
        )

    return (
        "<!doctype html>\n<html lang=\"en\">\n<head>\n"
        '<meta charset="utf-8">\n'
        '<meta name="viewport" content="width=device-width,initial-scale=1">\n'
        f"<title>{title} — editor</title>\n"
        f"<style>\n{BASE_CSS}\n\n/* theme */\n{theme}\n\n/* editor */\n{EDITOR_CSS}\n</style>\n"
        "</head>\n<body>\n"
        f'<div id="deck">\n{sections}\n</div>\n'
        f"<script>\n{EDITOR_JS}\n</script>\n"
        "</body>\n</html>\n"
    )


# Minimal fallback theme so the very first render (or an empty theme) still
# looks intentional. Real decks get a bespoke theme from the slide agent.
DEFAULT_THEME_CSS = """
body{background:radial-gradient(120% 120% at 0% 0%,#11151f 0%,#0a0b10 60%);
  color:#eef1f6}
.slide-inner{max-width:1040px}
.kicker{font:600 14px/1 system-ui;letter-spacing:.22em;text-transform:uppercase;
  color:#6ea8ff;margin-bottom:18px}
.slide-title{font:800 clamp(34px,6vmin,68px)/1.05 system-ui;letter-spacing:-.02em;
  margin-bottom:18px}
.slide-subtitle{font:600 clamp(20px,3vmin,30px)/1.2 system-ui;color:#aab3c5;
  margin-bottom:14px}
.slide-lead{font:400 clamp(17px,2.4vmin,24px)/1.5 system-ui;color:#c7cedb;
  max-width:880px}
.bullets{list-style:none;margin-top:22px;display:flex;flex-direction:column;gap:14px}
.bullets li{font:400 clamp(16px,2.3vmin,23px)/1.45 system-ui;padding-left:30px;
  position:relative;color:#dde3ee}
.bullets li::before{content:"";position:absolute;left:0;top:.62em;width:11px;height:11px;
  border-radius:3px;background:#6ea8ff}
.columns{display:flex;gap:34px;margin-top:26px}
.col{flex:1}
.big-number{font:800 clamp(50px,12vmin,120px)/1 system-ui;color:#6ea8ff}
.label{font:600 14px/1.2 system-ui;letter-spacing:.06em;text-transform:uppercase;
  color:#8b94a7}
.card{background:rgba(255,255,255,.04);border:1px solid rgba(255,255,255,.08);
  border-radius:16px;padding:24px 26px}
.quote{font:500 clamp(22px,3.4vmin,34px)/1.35 Georgia,serif;font-style:italic;
  color:#eef1f6}
.footer{position:absolute;bottom:6vmin;left:10vmin;right:10vmin;
  font:500 13px/1 system-ui;letter-spacing:.04em;color:#6b7384}
""".strip()


def _inject_images(html: str, images: dict[str, dict[str, str]]) -> str:
    """Replace `data-img="<id>"` placeholders with the real base64 src so the
    rendered deck is self-contained. A placeholder whose id we don't have (e.g.
    image generation failed) is marked `broken` and hidden by base CSS rather
    than left as a dead <img>."""
    def repl(m: "re.Match[str]") -> str:
        iid = m.group(1)
        img = images.get(iid)
        if img and img.get("data_uri"):
            # Keep data-img so the frontend/outline can still see which image.
            return f'data-img="{iid}" src="{img["data_uri"]}"'
        return f'data-img="{iid}" data-broken="1" class="slide-image broken"'
    return _IMG_PLACEHOLDER_RE.sub(repl, html)


def render_deck_html(deck: Deck) -> str:
    """Assemble a fully self-contained, presentable .html document."""
    theme = deck.theme_css.strip() or DEFAULT_THEME_CSS
    title = _html.escape(deck.title or "Untitled Deck")

    if deck.slides:
        sections = "\n".join(
            f'<section class="slide{" active" if i == 0 else ""}" '
            f'data-i="{i}" data-id="{_html.escape(s.id, quote=True)}">'
            f'<div class="slide-inner">{_inject_images(s.html, deck.images)}</div></section>'
            for i, s in enumerate(deck.slides)
        )
    else:
        sections = (
            '<section class="slide active"><div class="slide-inner">'
            '<div class="kicker">slide deck</div>'
            '<h1 class="slide-title">No slides yet</h1>'
            '<p class="slide-lead">Press <b>space</b> and describe the slides '
            'you want — e.g. &ldquo;make five slides about AI in space&rdquo;.</p>'
            '</div></section>'
        )

    return (
        "<!doctype html>\n<html lang=\"en\">\n<head>\n"
        '<meta charset="utf-8">\n'
        '<meta name="viewport" content="width=device-width,initial-scale=1">\n'
        f"<title>{title}</title>\n<style>\n{BASE_CSS}\n\n/* theme */\n{theme}\n</style>\n"
        "</head>\n<body>\n"
        f'<div id="deck">\n{sections}\n</div>\n'
        '<div class="slide-counter">1 / 1</div>\n'
        '<div class="deck-nav"><button data-prev>‹</button>'
        '<button data-next>›</button></div>\n'
        f"<script>\n{NAV_JS}\n</script>\n"
        "</body>\n</html>\n"
    )


__all__ = [
    "Slide", "Deck", "render_deck_html", "render_deck_editor_html",
    "SEMANTIC_CLASSES", "DEFAULT_THEME_CSS",
]
