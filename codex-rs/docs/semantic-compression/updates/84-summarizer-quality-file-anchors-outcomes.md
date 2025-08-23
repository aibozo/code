# 84 – Summarizer Quality: File Anchors & Outcomes

## Scope
- Improve compact summaries to include:
  - File anchors (top 2–3 paths central to the work).
  - Outcome cues (tests/build status, key errors resolved).
- Keep summaries concise and actionable; avoid raw diffs/logs.

## Approach
- Parse recent tool activity/diff text to extract paths (heuristics: look for `A/M/D`, `*** Update File:`, unified diff headers, or `apply_patch` metadata).
- Detect outcomes via regexes over assistant/tool output (e.g., `test result: ok`, `build failed`, `error:` lines).
- Render bullets:
  - `Files: src/foo.rs, tui/bar.rs`
  - `Result: tests passed` or `Result: fixed error E…`

## Acceptance Criteria
- Summaries remain within char budget.
- File anchors present when detectable; absent otherwise.
- Outcomes bullet present when detectable; absent otherwise.

