# Slide Creator

Build HTML slide decks by **talking**. Hold space, describe the slides you want,
and a voice agent (Azure OpenAI Realtime) relays your intent to a slide-designer
worker (gpt-5.4) that writes the deck. Follow up by voice or by typing — "slide 5
needs a chart", "make it all cohesive" — and watch the deck update live.

## How it works

```
browser  <--(ws, audio+JSON)-->  relay (FastAPI)  <--(wss)-->  Azure Realtime (voice)
                                      |
                                      └── SlideAgent (gpt-5.4 chat-completions) → deck
```

- **Voice agent** (`gpt-realtime`) is conversational only. It understands what you
  want and calls one tool, `build_slides`, with a plain-language instruction.
- **SlideAgent** (`gpt-5.4`, `voice_agent/slide_agent.py`) holds the deck and, on
  each instruction, rewrites the *complete* deck via a forced `write_deck` tool
  call. Create / edit-one-slide / reorder / restyle are all the same operation.
- **Deck model + renderer** (`voice_agent/deck.py`): slides are semantic HTML
  against a small fixed class vocabulary; all visual style lives in one
  `theme_css` blob. That separation makes "make it cohesive" cheap — regenerate
  one stylesheet and every slide restyles. The renderer emits a fully
  self-contained `.html` (deck nav, keyboard/click, postMessage bridge baked in).
- Each build is saved to **`slides/deck.html`** (self-contained — open or present
  anywhere) and pushed to the browser for live preview in a sandboxed iframe.

## Install

```bash
python -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt
cp .env.example .env   # fill in AZURE_OPENAI_API_KEY
```

## Run

```bash
uvicorn voice_agent.server:app --host 127.0.0.1 --port 8011
```

Open http://127.0.0.1:8011 — the voice line connects automatically. Hold **space**
(or the button) and say e.g. *"make five slides about AI in space; slide 1 is a
title, slide 2 …"*. Use the text box for typed instructions. The saved deck is at
`/slides/deck.html` (also linked as **Open ↗**).

## Config (`.env`)

| Var | Purpose |
|---|---|
| `AZURE_OPENAI_ENDPOINT` / `_DEPLOYMENT` / `_API_KEY` | Realtime voice model |
| `SLIDE_AZURE_OPENAI_DEPLOYMENT` / `_API_VERSION` | Slide-designer (chat-completions) model |
| `REALTIME_VOICE` | `alloy` / `echo` / `shimmer` / `coral` |
| `REALTIME_REASONING_EFFORT` | Realtime-2 only: `minimal`/`low`/`medium`/`high` (empty for v1) |

The old `DASHBOARD_AZURE_OPENAI_*` names are still accepted as fallbacks.

## Notes

- API key lives only on the relay; the browser never sees it.
- Generated decks (`slides/`) are gitignored.
- The deck mechanic (one slide visible, nav) is fixed in `BASE_CSS`; themes can't
  break it. Themes control everything else.
