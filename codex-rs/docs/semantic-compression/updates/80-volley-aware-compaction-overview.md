# 80 – Volley‑Aware Dynamic Compaction: Overview

## Summary
- Replace fixed-N message retention with volley‑aware, token‑pressure compaction.
- A volley is user → assistant (+ tools/results) as an atomic unit.
- Trigger compaction at token thresholds (75/85/95%), not counts.
- Summarize only untouched raw history (never summarize summaries).
- Two stages work together:
  - Post‑turn pruner (persisted summaries when memory is enabled).
  - Preflight compactor (ephemeral summaries to keep request ≤ model limit).

## Goals
- Preserve coherence by compacting volleys, not individual messages.
- Keep recent context rich; compact older spans with concise summaries.
- Never exceed model context window.
- No hard switches mid‑turn; compaction occurs post‑turn and/or preflight.

## Non‑Goals
- Changing model/provider behavior.
- Introducing network calls in the local summarizer.

## High‑Level Design
- Segment transcript into volleys by user messages; include following assistant/tool activity until next user message.
- Maintain a clear marker for summary items (e.g., `[memory:context v1 | repo=…]`).
- Post‑turn: when enabled, summarize pruned prefix (volley‑based) and persist. Keep K recent messages (tail selection still respects volley boundaries).
- Preflight: estimate tokens for the fully formatted prompt; if over budget, progressively replace oldest unsummarized volleys with compact summaries until under budget (with hysteresis targets).

## Triggers & Hysteresis
- Thresholds (configurable): 75% → summarize 1 volley; 85% → summarize until ≤ 70%; 95% → summarize aggressively (and drop non‑essential injections last).
- Hysteresis target: 70% default to avoid thrash.

## Summary Content
- Short title + bullets:
  - user intent; agent actions; key outcomes; file anchors.
- Exclude: full diffs/logs/test output; long code blocks.
- Size caps: per‑volley (e.g., 300–600 chars) and per request.

## Acceptance Criteria
- Large agent‑only sessions stay within context window without manual compact.
- Recent volley(s) remain uncompressed.
- Summaries are not summarized again.
- No regressions in token % left indicator.

## Sequencing
- See subsequent PR docs (81–86) for the implementation milestones.

