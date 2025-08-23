# PR: M7 — Research mode (optional)

Summary:
Allow the agent to perform scoped browsing and MCP-powered queries to gather context for planning. Emphasize reproducibility and citations.

Modes:
- Offline (default): no network; uses local docs/cache
- Online (explicit): network allowlist; all fetches logged and cached

Integration:
- Planner includes a `sources` section citing URLs + snippets
- Cached content stored under `artifacts/research/<ts>/`; re-used across attempts

Acceptance criteria:
- Clear flag to enable online mode; default remains offline
- Citation format and caching strategy documented
