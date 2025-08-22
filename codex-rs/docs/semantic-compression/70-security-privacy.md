# Security & Privacy Considerations

Scope

- Summaries and code hints can contain sensitive information if not filtered.
- The memory store is local to `~/.codex/` and must be readable only by the user.

Controls

1) Permissions
   - JSONL and sqlite files must be created with 0600 perms, mirroring `message_history.rs`.

2) Sensitive pattern filtering
   - Reuse the TODO hook in `message_history.rs` to add a shared helper: `is_sensitive_text(&str) -> bool`.
   - Before persisting any memory row, run this filter; if matched, drop persist and log a warning at debug level.

3) Opt‑in
   - Keep all features disabled by default. Document clear toggles in 20-config.md.

4) Truncation discipline
   - Summaries should be concise and avoid full secrets/keys; trim long code blocks to a few lines.

5) Deletion
   - Document a “wipe memory” step: delete `~/.codex/memory.jsonl` and/or `~/.codex/memory.sqlite`.

6) Network usage
   - If using OpenAI embeddings, respect the existing model/provider auth and config. Do not add new network calls outside approved providers.

