//! Deck data model + deterministic standalone-HTML renderer (Rust port of
//! deck.py). A deck is semantic slide content (against a fixed class
//! vocabulary) plus one `theme_css` blob, plus generated images stored by id.
//! The renderer assembles a fully self-contained .html: fixed deck mechanics
//! (one slide visible, keyboard/click nav, auto-fit scaler) + the theme +
//! inlined image data URIs.

use serde::Serialize;
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize)]
pub struct Slide {
    pub id: String,
    pub title: String,
    pub html: String,
}

#[derive(Clone, Debug)]
pub struct Image {
    pub data_uri: String,
    pub prompt: String,
}

#[derive(Clone, Debug, Default)]
pub struct Deck {
    pub title: String,
    pub theme_css: String,
    pub slides: Vec<Slide>,
    /// image id -> (data_uri, prompt). Ordered insertion preserved for the
    /// model-facing metadata list.
    pub images: Vec<(String, Image)>,
    seq: u32,
    img_seq: u32,
}

impl Deck {
    pub fn new() -> Self {
        Deck {
            title: "Untitled Deck".to_string(),
            ..Default::default()
        }
    }

    pub fn new_id(&mut self) -> String {
        self.seq += 1;
        format!("s{}", self.seq)
    }

    pub fn new_image_id(&mut self) -> String {
        self.img_seq += 1;
        format!("img{}", self.img_seq)
    }

    pub fn ensure_ids(&mut self) {
        for i in 0..self.slides.len() {
            if self.slides[i].id.is_empty() {
                self.seq += 1;
                self.slides[i].id = format!("s{}", self.seq);
            }
        }
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.slides.iter().position(|s| s.id == id)
    }

    pub fn image(&self, id: &str) -> Option<&Image> {
        self.images.iter().find(|(k, _)| k == id).map(|(_, v)| v)
    }

    pub fn insert_image(&mut self, id: String, img: Image) {
        self.images.push((id, img));
    }

    /// Insert or replace an image by id (upsert). Used to inject user-library
    /// images into the deck pool before a build without duplicating an id.
    pub fn set_image(&mut self, id: String, img: Image) {
        if let Some(slot) = self.images.iter_mut().find(|(k, _)| *k == id) {
            slot.1 = img;
        } else {
            self.images.push((id, img));
        }
    }

    /// Ids of every image actually referenced by a slide via `data-img="…"`.
    /// Used to prune unplaced library images so deck.json stays lean.
    pub fn referenced_image_ids(&self) -> std::collections::HashSet<String> {
        let mut ids = std::collections::HashSet::new();
        let needle = "data-img=\"";
        for s in &self.slides {
            let mut rest = s.html.as_str();
            while let Some(pos) = rest.find(needle) {
                let after = &rest[pos + needle.len()..];
                if let Some(end) = after.find('"') {
                    ids.insert(after[..end].to_string());
                    rest = &after[end + 1..];
                } else {
                    break;
                }
            }
        }
        ids
    }

    /// Remove images whose id is in `ids` (used to drop injected-but-unused
    /// library images after a build).
    pub fn retain_images_not_in(&mut self, ids: &std::collections::HashSet<String>) {
        self.images.retain(|(id, _)| !ids.contains(id));
    }

    /// Model-facing JSON: slides + theme + image METADATA only (never the
    /// base64), so the model context stays small.
    pub fn to_model_json(&self) -> Value {
        json!({
            "title": self.title,
            "theme_css": self.theme_css,
            "slides": self.slides.iter().map(|s| json!({
                "id": s.id, "title": s.title, "html": s.html,
            })).collect::<Vec<_>>(),
            "images": self.images.iter().map(|(id, img)| json!({
                "id": id, "prompt": img.prompt,
            })).collect::<Vec<_>>(),
        })
    }

    /// Frontend-facing JSON for the live preview (outline etc.).
    pub fn to_ui_json(&self) -> Value {
        json!({
            "title": self.title,
            "slides": self.slides.iter().map(|s| json!({
                "id": s.id, "title": s.title,
            })).collect::<Vec<_>>(),
        })
    }

    /// Complete, round-trippable snapshot — INCLUDING image base64 and the id
    /// counters — for persisting to `deck.json`. This is the shared source of
    /// truth: both the AI agent and the manual /editor read & write it, so a
    /// manual edit survives a later AI build (the agent reloads this first).
    /// Distinct from `to_model_json`, which omits the heavy base64.
    pub fn to_full_json(&self) -> Value {
        json!({
            "title": self.title,
            "theme_css": self.theme_css,
            "slides": self.slides.iter().map(|s| json!({
                "id": s.id, "title": s.title, "html": s.html,
            })).collect::<Vec<_>>(),
            "images": self.images.iter().map(|(id, img)| json!({
                "id": id, "data_uri": img.data_uri, "prompt": img.prompt,
            })).collect::<Vec<_>>(),
            "_seq": self.seq,
            "_img_seq": self.img_seq,
        })
    }

    /// Rebuild a Deck from a `to_full_json` snapshot. Tolerant of missing keys
    /// so a hand-edited or partial deck.json still loads. Image entries accept
    /// either the object form `{id,data_uri,prompt}` (our output) or a
    /// `{id: {data_uri,prompt}}` map (Python's `images` dict shape).
    pub fn from_full_json(v: &Value) -> Deck {
        let mut deck = Deck::new();
        if let Some(t) = v.get("title").and_then(|x| x.as_str()) {
            if !t.is_empty() {
                deck.title = t.to_string();
            }
        }
        deck.theme_css = v.get("theme_css").and_then(|x| x.as_str()).unwrap_or("").to_string();

        if let Some(arr) = v.get("slides").and_then(|x| x.as_array()) {
            for s in arr {
                deck.slides.push(Slide {
                    id: s.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    title: s.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    html: s.get("html").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                });
            }
        }

        match v.get("images") {
            // Our array form: [{id, data_uri, prompt}, …]
            Some(Value::Array(arr)) => {
                for img in arr {
                    let id = img.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    if id.is_empty() {
                        continue;
                    }
                    deck.images.push((id, Image {
                        data_uri: img.get("data_uri").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        prompt: img.get("prompt").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    }));
                }
            }
            // Python dict form: {id: {data_uri, prompt}, …}
            Some(Value::Object(map)) => {
                for (id, img) in map {
                    deck.images.push((id.clone(), Image {
                        data_uri: img.get("data_uri").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        prompt: img.get("prompt").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    }));
                }
            }
            _ => {}
        }

        // Restore counters; if absent (hand-written file) or too low, derive a
        // safe high-water mark from existing ids so new ids never collide.
        deck.seq = v.get("_seq").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        deck.img_seq = v.get("_img_seq").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        deck.seq = deck.seq.max(max_numeric_suffix(deck.slides.iter().map(|s| s.id.as_str())));
        deck.img_seq = deck.img_seq.max(max_numeric_suffix(deck.images.iter().map(|(id, _)| id.as_str())));
        deck.ensure_ids();
        deck
    }
}

/// Highest trailing integer across an iterator of ids (0 if none), e.g.
/// "s7" -> 7, "img12" -> 12.
fn max_numeric_suffix<'a>(ids: impl Iterator<Item = &'a str>) -> u32 {
    let mut best = 0u32;
    for id in ids {
        let digits: String = id.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            if let Ok(n) = digits.chars().rev().collect::<String>().parse::<u32>() {
                best = best.max(n);
            }
        }
    }
    best
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Replace `data-img="<id>"` placeholders with the real base64 src so the
/// rendered deck is self-contained. Missing id -> marked broken (hidden).
fn inject_images(html: &str, deck: &Deck) -> String {
    // Lightweight scan for the literal `data-img="ID"`; no regex dependency.
    let mut out = String::with_capacity(html.len());
    let needle = "data-img=\"";
    let mut rest = html;
    while let Some(pos) = rest.find(needle) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + needle.len()..];
        if let Some(end) = after.find('"') {
            let id = &after[..end];
            match deck.image(id) {
                Some(img) => {
                    out.push_str(&format!("data-img=\"{id}\" src=\"{}\"", img.data_uri));
                }
                None => {
                    out.push_str(&format!(
                        "data-img=\"{id}\" data-broken=\"1\" class=\"slide-image broken\""
                    ));
                }
            }
            rest = &after[end + 1..];
        } else {
            // Unterminated; emit verbatim and stop.
            out.push_str(needle);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

pub fn render_deck_html(deck: &Deck) -> String {
    let theme = if deck.theme_css.trim().is_empty() {
        DEFAULT_THEME_CSS
    } else {
        deck.theme_css.trim()
    };
    let title = esc(if deck.title.is_empty() {
        "Untitled Deck"
    } else {
        &deck.title
    });

    let sections = if deck.slides.is_empty() {
        "<section class=\"slide active\"><div class=\"slide-inner\">\
<div class=\"kicker\">slide deck</div>\
<h1 class=\"slide-title\">No slides yet</h1>\
<p class=\"slide-lead\">Press <b>space</b> and describe the slides you want.</p>\
</div></section>"
            .to_string()
    } else {
        deck.slides
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let active = if i == 0 { " active" } else { "" };
                format!(
                    "<section class=\"slide{active}\" data-i=\"{i}\" data-id=\"{id}\">\
<div class=\"slide-inner\">{body}</div></section>",
                    id = esc(&s.id),
                    body = inject_images(&s.html, deck),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n\
<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
<title>{title}</title>\n<style>\n{base}\n\n/* theme */\n{theme}\n</style>\n\
</head>\n<body>\n\
<div id=\"deck\">\n{sections}\n</div>\n\
<div class=\"slide-counter\">1 / 1</div>\n\
<div class=\"deck-nav\"><button data-prev>\u{2039}</button><button data-next>\u{203a}</button></div>\n\
<script>\n{nav}\n</script>\n\
</body>\n</html>\n",
        base = BASE_CSS,
        nav = NAV_JS,
    )
}

/// Assemble a PRINT/PDF document: every slide laid out as its own 16:9 page,
/// no nav chrome, no one-at-a-time hiding. Reuses the exact slide markup + theme
/// as the live deck so the PDF matches what the user sees. Intended to be loaded
/// in a hidden iframe and sent to `window.print()` ("Save as PDF").
pub fn render_deck_pdf_html(deck: &Deck) -> String {
    let theme = if deck.theme_css.trim().is_empty() {
        DEFAULT_THEME_CSS
    } else {
        deck.theme_css.trim()
    };
    let title = esc(if deck.title.is_empty() { "Untitled Deck" } else { &deck.title });

    let sections = deck
        .slides
        .iter()
        .map(|s| {
            format!(
                "<section class=\"slide\"><div class=\"slide-inner\">{body}</div></section>",
                body = inject_images(&s.html, deck),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Print CSS: a fixed 1280x720 page per slide. We OVERRIDE the base
    // mechanics (which absolutely-position + hide non-active slides) so all
    // slides stack as printable pages. `print-color-adjust:exact` keeps theme
    // backgrounds; the theme's own .slide-inner/typography styles still apply.
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n\
<meta charset=\"utf-8\">\n<title>{title}</title>\n<style>\n{base}\n\n\
/* theme */\n{theme}\n\n\
/* print layout — one 16:9 page per slide, overrides deck mechanics */\n\
@page{{size:1280px 720px;margin:0}}\n\
html,body{{width:auto;height:auto;overflow:visible;margin:0;padding:0}}\n\
#deck{{position:static;width:auto;height:auto}}\n\
.slide,.slide:not(.active){{position:relative!important;display:flex!important;\
width:1280px;height:720px;inset:auto;page-break-after:always;break-after:page;\
overflow:hidden;animation:none!important;transform:none!important;\
-webkit-print-color-adjust:exact;print-color-adjust:exact}}\n\
.slide:last-child{{page-break-after:auto;break-after:auto}}\n\
.slide-inner{{transform:none!important}}\n\
</style>\n</head>\n<body>\n<div id=\"deck\">\n{sections}\n</div>\n\
<script>\n{print_js}\n</script>\n</body>\n</html>\n",
        base = BASE_CSS,
        print_js = PDF_AUTOPRINT_JS,
    )
}

/// Auto-open the print dialog once the page (and its images) have loaded. This
/// runs in the user's DEFAULT BROWSER — where window.print() works and offers
/// "Save as PDF" — because the Tauri/WKWebView itself ignores window.print().
/// `?print=1` opts in; opening the page without it just shows the deck.
const PDF_AUTOPRINT_JS: &str = r#"(function(){
  if(!/[?&]print=1/.test(location.search))return;
  function go(){ setTimeout(function(){ try{window.print()}catch(e){} }, 350); }
  var imgs=[].slice.call(document.images).filter(function(i){return !i.complete});
  if(!imgs.length){ if(document.readyState==='complete')go(); else window.addEventListener('load',go); return; }
  var left=imgs.length, fired=false;
  function one(){ if(--left<=0 && !fired){fired=true;go()} }
  imgs.forEach(function(i){ i.addEventListener('load',one); i.addEventListener('error',one); });
  setTimeout(function(){ if(!fired){fired=true;go()} }, 5000); // fallback
})();"#;

/// Assemble the WYSIWYG editor document. Same slide markup + BASE_CSS + theme
/// as `render_deck_html` (so editing matches the final look exactly), but the
/// editor runtime + overlay CSS replace the presentation nav. Served
/// same-origin (no sandbox) so the /editor page can attach to it; all editing
/// still flows through postMessage, keeping the contract identical to preview.
pub fn render_deck_editor_html(deck: &Deck) -> String {
    let theme = if deck.theme_css.trim().is_empty() {
        DEFAULT_THEME_CSS
    } else {
        deck.theme_css.trim()
    };
    let title = esc(if deck.title.is_empty() {
        "Untitled Deck"
    } else {
        &deck.title
    });

    let sections = if deck.slides.is_empty() {
        "<section class=\"slide active\"><div class=\"slide-inner\">\
<h1 class=\"slide-title\">No slides yet</h1></div></section>"
            .to_string()
    } else {
        deck.slides
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let active = if i == 0 { " active" } else { "" };
                format!(
                    "<section class=\"slide{active}\" data-i=\"{i}\" data-id=\"{id}\">\
<div class=\"slide-inner\">{body}</div></section>",
                    id = esc(&s.id),
                    body = inject_images(&s.html, deck),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n\
<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
<title>{title} \u{2014} editor</title>\n\
<style>\n{base}\n\n/* theme */\n{theme}\n\n/* editor */\n{editor}\n</style>\n\
</head>\n<body>\n\
<div id=\"deck\">\n{sections}\n</div>\n\
<script>\n{js}\n</script>\n\
</body>\n</html>\n",
        base = BASE_CSS,
        editor = EDITOR_CSS,
        js = EDITOR_JS,
    )
}

pub const BASE_CSS: &str = r#"*{box-sizing:border-box;margin:0;padding:0}
html,body{height:100%;width:100%;overflow:hidden}
body{font-family:system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;
  background:#0b0c10;color:#e7e7ea}
#deck{position:relative;height:100vh;width:100vw}
.slide{position:absolute;inset:0;flex-direction:column;
  justify-content:center;padding:7vmin 10vmin;overflow:hidden}
.slide:not(.active){display:none!important}
.slide.active{animation:slidein .35s ease}
@keyframes slidein{from{opacity:0;transform:translateY(14px)}to{opacity:1;transform:none}}
.slide-inner{width:100%;max-width:1100px;margin:0 auto;
  transform-origin:center center;transition:transform .2s ease}
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
  letter-spacing:.08em;opacity:.45;z-index:20}"#;

pub const NAV_JS: &str = r#"(function(){
  var slides=[].slice.call(document.querySelectorAll('.slide'));
  var i=0;
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
})();"#;

pub const DEFAULT_THEME_CSS: &str = r#"body{background:radial-gradient(120% 120% at 0% 0%,#11151f 0%,#0a0b10 60%);
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
  font:500 13px/1 system-ui;letter-spacing:.04em;color:#6b7384}"#;

// ——— Editor mode ————————————————————————————————————————————————————————
// A second runtime, used by the /editor page. The slide markup + BASE_CSS +
// theme are IDENTICAL to the presentable render (true WYSIWYG); only the nav
// script is swapped for an editing script, with editor-only CSS appended.

pub const EDITOR_CSS: &str = r#"html,body{overflow:auto!important}
.slide{cursor:default}
.slide-inner [data-ed-hover]{outline:1.5px dashed rgba(110,168,255,.6);outline-offset:3px}
.slide-inner [data-ed-sel]{outline:2px solid #6ea8ff!important;outline-offset:3px;
  border-radius:3px}
.slide-inner [contenteditable=true]{cursor:text;outline:2px solid #2ecc71!important;
  outline-offset:3px}
.slide-image{cursor:pointer}
.deck-nav,.slide-counter{display:none!important}"#;

// Editor runtime — talks to the parent /editor page entirely via postMessage:
//   parent → iframe:  {type:'ed-show', i} / {type:'ed-style', prop, value} /
//                     {type:'ed-clear-style'} / {type:'ed-set-image', dataUri, imgId} /
//                     {type:'ed-serialize'}
//   iframe → parent:  {type:'ed-ready', n} / {type:'ed-select', info|null} /
//                     {type:'ed-deck', slides:[{i,html}]}
pub const EDITOR_JS: &str = r#"(function(){
  var slides=[].slice.call(document.querySelectorAll('.slide'));
  var i=0, sel=null;
  function show(n){
    i=Math.max(0,Math.min(slides.length-1,n));
    slides.forEach(function(s,k){s.classList.toggle('active',k===i)});
  }
  function selectable(el){
    if(!el) return null;
    var inner=el.closest('.slide-inner'); if(!inner) return null;
    if(el===inner){ return null; }
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
      inline: el.getAttribute('style')||''
    };
  }
  function rgbToHex(c){
    if(!c) return '';
    var m=c.match(/rgba?\(([^)]+)\)/); if(!m) return c;
    var p=m[1].split(',').map(function(x){return parseFloat(x)});
    if(p.length>=4 && p[3]===0) return '';
    function h(n){return ('0'+Math.round(n).toString(16)).slice(-2)}
    return '#'+h(p[0])+h(p[1])+h(p[2]);
  }
  function postSel(){
    try{parent.postMessage({type:'ed-select',info:describe(sel),i:i},'*')}catch(e){}
  }

  document.addEventListener('click',function(e){
    var el=selectable(e.target);
    if(sel && sel.getAttribute('contenteditable')==='true' && sel.contains(e.target)) return;
    if(el===sel){ return; }
    clearSel();
    if(el){ sel=el; sel.setAttribute('data-ed-sel','1'); }
    postSel();
  });

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

  document.addEventListener('mouseover',function(e){
    var el=selectable(e.target); if(!el) return;
    if(el.getAttribute('contenteditable')==='true') return;
    el.setAttribute('data-ed-hover','1');
  });
  document.addEventListener('mouseout',function(e){
    if(e.target&&e.target.removeAttribute) e.target.removeAttribute('data-ed-hover');
  });

  function cleanSlide(slide){
    var inner=slide.querySelector('.slide-inner'); if(!inner) return '';
    var c=inner.cloneNode(true);
    [].forEach.call(c.querySelectorAll('[data-ed-sel],[data-ed-hover],[contenteditable]'),
      function(n){ n.removeAttribute('data-ed-sel'); n.removeAttribute('data-ed-hover');
        n.removeAttribute('contenteditable'); });
    [].forEach.call(c.querySelectorAll('img'),function(im){
      if(im.getAttribute('data-img')){ im.removeAttribute('src'); }
      im.classList.remove('broken'); im.removeAttribute('data-broken');
    });
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
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Deck {
        let mut d = Deck::new();
        d.title = "Test".into();
        d.theme_css = "body{color:red}".into();
        d.slides = vec![
            Slide { id: "s1".into(), title: "One".into(), html: "<h1 class=\"slide-title\">Hello</h1>".into() },
            Slide { id: "s2".into(), title: "Two".into(), html: "<img class=\"slide-image\" data-img=\"img1\">".into() },
        ];
        d.insert_image("img1".into(), Image { data_uri: "data:image/png;base64,AAAA".into(), prompt: "x".into() });
        d.seq = 2;
        d.img_seq = 1;
        d
    }

    #[test]
    fn full_json_round_trips() {
        let d = sample();
        let d2 = Deck::from_full_json(&d.to_full_json());
        assert_eq!(d2.title, "Test");
        assert_eq!(d2.slides.len(), 2);
        assert_eq!(d2.slides[0].id, "s1");
        assert_eq!(d2.seq, 2);
        assert_eq!(d2.img_seq, 1);
        assert_eq!(d2.image("img1").unwrap().data_uri, "data:image/png;base64,AAAA");
    }

    #[test]
    fn counters_recover_from_ids_when_absent() {
        let v = json!({
            "slides": [{"id": "s7", "title": "a", "html": "b"}],
            "images": [{"id": "img4", "data_uri": "x", "prompt": ""}],
        });
        let d = Deck::from_full_json(&v);
        // New ids must not collide with existing s7 / img4.
        let mut d = d;
        assert_eq!(d.new_id(), "s8");
        assert_eq!(d.new_image_id(), "img5");
    }

    #[test]
    fn accepts_python_images_dict_shape() {
        let v = json!({
            "title": "P",
            "slides": [],
            "images": {"img1": {"data_uri": "data:abc", "prompt": "p"}},
        });
        let d = Deck::from_full_json(&v);
        assert_eq!(d.image("img1").unwrap().data_uri, "data:abc");
    }

    #[test]
    fn image_pool_upsert_reference_and_prune() {
        let mut d = Deck::new();
        d.slides = vec![Slide {
            id: "s1".into(),
            title: "T".into(),
            html: "<img class=\"slide-image\" data-img=\"lib1\">".into(),
        }];
        // Upsert two library images; one is referenced by the slide, one isn't.
        d.set_image("lib1".into(), Image { data_uri: "data:a".into(), prompt: "used".into() });
        d.set_image("lib2".into(), Image { data_uri: "data:b".into(), prompt: "unused".into() });
        // set_image upserts, not duplicates.
        d.set_image("lib1".into(), Image { data_uri: "data:a2".into(), prompt: "used2".into() });
        assert_eq!(d.images.len(), 2);
        assert_eq!(d.image("lib1").unwrap().data_uri, "data:a2");

        let referenced = d.referenced_image_ids();
        assert!(referenced.contains("lib1"));
        assert!(!referenced.contains("lib2"));

        // Prune the unreferenced lib image.
        let unused: std::collections::HashSet<String> =
            ["lib2".to_string()].into_iter().collect();
        d.retain_images_not_in(&unused);
        assert_eq!(d.images.len(), 1);
        assert!(d.image("lib1").is_some());
        assert!(d.image("lib2").is_none());
    }

    #[test]
    fn editor_render_has_runtime_export_does_not() {
        let d = sample();
        let eh = render_deck_editor_html(&d);
        assert!(eh.contains("ed-serialize"));
        assert!(eh.contains("contenteditable"));
        // Image injected for true WYSIWYG.
        assert!(eh.contains("data-img=\"img1\" src=\"data:image/png;base64,AAAA\""));
        let ph = render_deck_html(&d);
        assert!(!ph.contains("ed-serialize"));
        assert!(ph.contains("deck-nav"));
    }
}
