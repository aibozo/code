# 86 – Protecting PR Docs & Pinning Important Context

## Goals
- Ensure PR plan docs and other critical design texts are never summarized away.
- Provide an ergonomic way to “pin” documents so they persist at full fidelity in context.

## Automatic Protection
- Heuristics to identify PR docs:
  - Path: `docs/**` with filenames containing `pr`, `plan`, `rfc`, `design`.
  - Content: first heading starting with `# PR`, `# Plan`, `# RFC`, or a marker like `[DOC:PR]`.
- Preflight compactor & pruner rules:
  - Do not select protected docs as compaction candidates.
  - Keep them intact until the absolute last resort (after all other compaction avenues fail).

## Pinning Tool (optional, complementary)
- New tool: `pin_context`
  - Args: `{ paths?: string[], text?: string, ttl_turns?: number }`
  - Behavior: injects items into a “pinned” section that is excluded from compaction.
  - TTL optional; default sticks until user unpins.
- UI affordance: minimal; pinning can be done by the model when user requests or via quick command.

## Acceptance Criteria
- PR docs included in context remain unaltered across compaction events.
- Pinned items consistently appear in the prompt and are excluded from summaries.
- No regressions in token budget enforcement.

