"""Realtime voice-agent tools + persona for the slide creator.

The voice agent's whole job is conversational: listen to what the user wants,
then hand a clear natural-language instruction to the slide-building worker via
the ``build_slides`` tool. It does NOT write HTML or CSS itself — the SlideAgent
(gpt-5.4) does that. The voice agent just relays intent and narrates progress.

Mirrors the old request/ack/inject pattern: the tool returns a synchronous
"pending" ack so the agent keeps talking naturally; the relay runs the slow
build in the background and injects a ``[BUILD RESULT]`` message when done.
"""
from __future__ import annotations


BUILD_SLIDES_TOOL = {
    "type": "function",
    "name": "build_slides",
    "description": (
        "Create or modify the slide deck. Call this whenever the user asks to "
        "make, add, change, reorder, restyle, or fix slides. Pass the user's "
        "full intent in plain language — the slide designer that receives it "
        "writes the actual HTML/CSS, so be specific and complete. "
        "Examples of good instructions: "
        "'Create 5 slides about AI in space: slide 1 is a title/intro, slide 2 "
        "explains that space is vast and AI can help explore it, slide 3 …'; "
        "'Change slide 5 to focus on the cost of launches with a few bullet "
        "points'; 'Make the whole deck cohesive — unify the colours, fonts, and "
        "spacing into one clean theme'. "
        "Returns immediately with a pending acknowledgement; the finished deck "
        "arrives shortly as a [BUILD RESULT] message and is shown on screen."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "instruction": {
                "type": "string",
                "description": (
                    "The complete instruction for the slide designer, in plain "
                    "language. Include slide counts, per-slide topics, and any "
                    "style direction the user gave. Preserve the user's "
                    "specifics; do not summarize them away."
                ),
            },
        },
        "required": ["instruction"],
    },
}


BASE_INSTRUCTIONS = (
    "You are a friendly, fast voice assistant that helps the user build an HTML "
    "slide deck by talking. Speak concisely and naturally. "
    "You do NOT write HTML or CSS yourself — a separate slide designer does that. "
    "Your job is to understand what the user wants and call the build_slides tool "
    "with a clear, complete instruction in plain language, preserving their "
    "specifics (how many slides, what each slide is about, any style direction). "
    "When the user describes multiple slides at once, capture ALL of them in a "
    "single build_slides call. When they ask to tweak one slide or restyle the "
    "whole deck, pass that as the instruction too. "
    "The tool returns a pending acknowledgement immediately — tell the user "
    "briefly what you're building (one short sentence), then stop and wait. "
    "When a [BUILD RESULT] message arrives, confirm what changed in one short "
    "sentence and invite the next change. Do not read slide markup aloud. "
    "If the user is just chatting or asking what you can do, answer briefly and "
    "suggest they describe the slides they want."
)


__all__ = ["BUILD_SLIDES_TOOL", "BASE_INSTRUCTIONS"]
