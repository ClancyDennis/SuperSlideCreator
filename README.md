# Super Slide Creator

Build HTML slide decks by **talking**. Hold the mic, describe the slides you want,
and a realtime voice agent relays your intent to a slide-designer model that writes
the deck. Follow up by voice or by typing — *"slide 5 needs a chart"*, *"make it all
cohesive"* — and watch the deck rebuild live.

Ships two ways:

- **macOS desktop app** (`src-tauri/`) — a self-contained Tauri app. Bring your own
  OpenAI or Azure OpenAI key, entered in-app. Nothing to host.
- **Browser + Python relay** (`voice_agent/`) — run a local FastAPI server and use it
  from a browser. Good for development or non-mac use.

Both share the same web frontend (`web/`) and the same idea below.

## How it works

```
frontend  <--(ws: audio + JSON)-->  relay  <--(wss)-->  Realtime voice model
                                       |
                                       └── SlideAgent (chat-completions) → deck
```

- **Voice agent** (`gpt-realtime`) is conversational only. It understands what you
  want and calls one tool, `build_slides`, with a plain-language instruction.
- **SlideAgent** holds the deck and, on each instruction, rewrites the *complete*
  deck via a forced `write_deck` tool call. Create / edit-one-slide / reorder /
  restyle are all the same operation.
- **Deck model + renderer**: slides are semantic HTML against a small fixed class
  vocabulary; all visual style lives in one `theme_css` blob. That separation makes
  "make it cohesive" cheap — regenerate one stylesheet and every slide restyles. The
  renderer emits a fully self-contained `.html` (deck nav, keyboard/click,
  postMessage bridge baked in) you can open or present anywhere.
- A **manual editor** (`web/editor.*`) lets you tweak the deck by hand against the
  same contract the voice preview uses.

The relay logic exists in two implementations that mirror each other: Rust
(`src-tauri/src/`) for the desktop app, Python (`voice_agent/`) for the browser path.

## Option A — macOS desktop app

**Download:** grab the latest signed, notarized `.dmg` from
[Releases](https://github.com/ClancyDennis/SuperSlideCreator/releases) (Apple Silicon),
drag to Applications, and open. On first launch macOS asks for **microphone**
permission — allow it for voice.

Then open **Settings** in the app and paste your key:

- **OpenAI** (default): an OpenAI API key with access to a realtime model, a
  chat-completions model, and an image model.
- **Azure OpenAI**: your resource endpoint + `api-key`, with deployments for each.

The key is stored in your user config dir and never leaves your machine except to
call the model provider directly.

### Build it yourself

Requires [Rust](https://rustup.rs) and the [Tauri prerequisites](https://tauri.app/start/prerequisites/).

```bash
cd src-tauri
cargo tauri dev      # run in development
cargo tauri build    # produce a release .app + .dmg under target/release/bundle/
```

(A release build signs with a Developer ID if configured in `tauri.conf.json`; remove
the `macOS.signingIdentity` field to build unsigned for local use.)

## Option B — browser + Python relay

```bash
python -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt
cp .env.example .env        # fill in your Azure OpenAI key + endpoint
uvicorn voice_agent.server:app --host 127.0.0.1 --port 8011
```

Open <http://127.0.0.1:8011> — the voice line connects automatically. Hold **space**
(or the button) and say e.g. *"make five slides about AI in space; slide 1 is a
title, slide 2 …"*. Use the text box for typed instructions. The saved deck is at
`/slides/deck.html` (also linked as **Open ↗**).

### Config (`.env`)

| Var | Purpose |
|---|---|
| `AZURE_OPENAI_ENDPOINT` / `_DEPLOYMENT` / `_API_KEY` | Realtime voice model |
| `AZURE_OPENAI_API_VERSION` | Realtime API version |
| `SLIDE_AZURE_OPENAI_DEPLOYMENT` / `_API_VERSION` | Slide-designer (chat-completions) model |
| `IMAGE_AZURE_OPENAI_DEPLOYMENT` / `_API_VERSION` | Image-generation model |
| `REALTIME_VOICE` | `alloy` / `echo` / `shimmer` / `coral` |
| `REALTIME_REASONING_EFFORT` | Realtime-2 only: `minimal`/`low`/`medium`/`high` (empty for v1) |

## Notes

- The API key lives only on the relay (desktop config or Python server); the frontend
  never sees it.
- Generated decks (`slides/`) and the app's image library are gitignored / kept in the
  user data dir.
- The deck mechanic (one slide visible, nav) is fixed in the base CSS; themes control
  everything else and can't break navigation.
