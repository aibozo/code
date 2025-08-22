# Config Shape & Defaults

TOML Schema Additions

```toml
[memory]
enabled = false
summarize_on_prune = true

[memory.inject]
max_items = 2
max_chars = 500

[memory.embedding]
enabled = false
provider = "openai" # or "local"
top_k = 5
dim = 3072

[memory.code_index]
enabled = false
chunk_bytes = 1500
top_k = 5
```

Rust Types (exact names)

- `codex-rs/core/src/config_types.rs`
  - `pub struct MemoryInjectConfig { pub max_items: usize, pub max_chars: usize }`
  - `pub struct MemoryEmbeddingConfig { pub enabled: bool, pub provider: String, pub top_k: usize, pub dim: usize }`
  - `pub struct MemoryCodeIndexConfig { pub enabled: bool, pub chunk_bytes: usize, pub top_k: usize }`
  - `pub struct MemoryConfig { pub enabled: bool, pub summarize_on_prune: bool, pub inject: MemoryInjectConfig, pub embedding: MemoryEmbeddingConfig, pub code_index: MemoryCodeIndexConfig }`
- `codex-rs/core/src/config.rs`
  - add `memory: MemoryConfig` to `Config`, load/override with defaults as above.

Surfacing (optional)

- `codex-rs/common/src/config_summary.rs` — append entries:
  - `memory`: "disabled" or `enabled (inject=<N>/<M> chars, embed=<on|off>, code-index=<on|off>)`

Environment Variables (optional convenience)

- `CODEX_MEMORY_ENABLED=1`
- `CODEX_MEMORY_EMBEDDING_PROVIDER=openai|local`

CLI Flags (future)

- Keep Phase 1 entirely config‑file driven to limit surface area. CLI/UI toggles can be added in a later pass.

