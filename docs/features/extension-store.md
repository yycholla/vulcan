# Extension Store / Repository — Design & Implementation Plan

This document describes how to add an extension store and repository system to Vulcan, enabling third-party extensions to be discovered, installed, verified, and loaded safely at runtime.

> **Prerequisite context:** See [`docs/features/extensions.md`](./extensions.md) for the lighter-weight skill→extension promotion path that feeds into this system. This document assumes extensions already exist as a concept and focuses on the packaging, distribution, and dynamic-loading infrastructure.

---

## 1. Extension Model & Interface

### Core Trait Definition

```rust
// src/extension/mod.rs
pub trait Extension: Send + Sync {
    /// Metadata about the extension
    fn metadata(&self) -> &ExtensionMetadata;

    /// Initialize the extension (called once on load)
    fn initialize(&self, ctx: &ExtensionContext) -> Result<()>;

    /// Optional: cleanup on unload
    fn shutdown(&self) -> Result<()>;

    /// What capabilities this extension provides
    fn capabilities(&self) -> &[Capability];
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ExtensionMetadata {
    pub id: String,                // e.g. "memory@redis"
    pub name: String,
    pub version: Version,          // semver
    pub description: String,
    pub author: String,
    pub license: String,
    pub repository: Option<String>,
    pub signature: Option<String>, // cryptographic signature
}

#[derive(Clone, Serialize, Deserialize)]
pub enum Capability {
    ToolProvider(String),      // Provides a tool
    MemoryBackend(String),     // Custom memory storage
    ProviderHook(String),      // Hooks into provider calls
    EventHandler(String),      // Responds to events
    CustomAgent(String),       // New agent type
}
```

---

## 2. Extension Package Format

### Directory Structure

```
my-extension/
├── extension.toml          # Manifest (required)
├── extension.wasm          # Or .so/.dylib/.dll (compiled)
├── extension.js            # Or .py (scripted)
├── schema.json             # Tool/API schemas
├── README.md
└── LICENSE
```

### Manifest (`extension.toml`)

```toml
id = "memory@redis"
name = "Redis Memory Backend"
version = "1.0.0"
description = "Store agent memory in Redis"
author = "Jane Doe"
license = "MIT"
language = "rust"            # or "wasm", "js", "python"
entry = "libmemory_redis.so" # or "extension.wasm"
signature = "sha256:abc123..."

[capabilities]
memory_backend = "redis"

[dependencies]
redis = "0.23"
serde = "1.0"
```

---

## 3. Extension Repository System

### Repository Index

```rust
// src/extension/store.rs
#[derive(Clone, Serialize, Deserialize)]
pub struct RepositoryIndex {
    pub name: String,
    pub url: String,                  // Base URL
    pub extensions: Vec<ExtensionRecord>,
    pub last_updated: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ExtensionRecord {
    pub id: String,
    pub name: String,
    pub version: Version,
    pub description: String,
    pub download_url: String,
    pub checksum: String,             // SHA-256
    pub signature: String,            // GPG/ed25519 signature
    pub min_vulcan_version: Option<String>,
    pub max_vulcan_version: Option<String>,
}
```

### Store Manager

```rust
pub struct ExtensionStore {
    repo_urls: Vec<String>,
    install_dir: PathBuf,
    loaded_extensions: RwLock<HashMap<String, Box<dyn Extension>>>,
    trusted_keys: Vec<PublicKey>,     // For signature verification
}

impl ExtensionStore {
    /// Discover extensions from configured repositories
    pub async fn discover(&self, filter: Filter) -> Result<Vec<ExtensionRecord>> {
        let mut all = vec![];
        for url in &self.repo_urls {
            let index: RepositoryIndex = self.fetch_index(url).await?;
            all.extend(index.extensions);
        }
        Ok(all.into_iter().filter(|e| filter.matches(e)).collect())
    }

    /// Download, verify, and install an extension
    pub async fn install(&self, record: &ExtensionRecord) -> Result<()> {
        // 1. Download package
        let pkg_bytes = self.download(&record.download_url).await?;

        // 2. Verify checksum
        let computed = sha2::Sha256::digest(&pkg_bytes);
        if format!("sha256:{}", hex::encode(computed)) != record.checksum {
            anyhow::bail!("Checksum mismatch");
        }

        // 3. Verify signature
        self.verify_signature(&pkg_bytes, &record.signature)?;

        // 4. Extract to install dir
        let install_path = self.install_dir.join(&record.id);
        tokio::fs::create_dir_all(&install_path).await?;
        self.extract(&pkg_bytes, &install_path).await?;

        // 5. Record installation
        self.record_installation(record, &install_path).await?;

        Ok(())
    }

    /// Load an installed extension
    pub fn load(&self, id: &str) -> Result<()> {
        let manifest = self.read_manifest(id)?;

        match manifest.language.as_str() {
            "rust"  => self.load_native(id, &manifest),
            "wasm"  => self.load_wasm(id, &manifest),
            "js"    => self.load_javascript(id, &manifest),
            "python" => self.load_python(id, &manifest),
            _ => anyhow::bail!("Unsupported language"),
        }
    }
}
```

---

## 4. Runtime Loading Strategies

### Strategy A: Native (Rust) — Dynamic Libraries

```rust
impl ExtensionStore {
    fn load_native(&self, id: &str, manifest: &Manifest) -> Result<()> {
        let lib_path = self.install_dir.join(id).join(&manifest.entry);

        // SAFETY: Library signature was already verified.
        // Loading untrusted code can still compromise the process.
        // For stronger isolation, use WASM or a child process.
        let lib = unsafe { libloading::Library::new(lib_path)? };

        let factory: libloading::Symbol<
            unsafe extern fn() -> *mut dyn Extension,
        > = unsafe { lib.get(b"create_extension") }?;

        let extension = unsafe { Box::from_raw(factory()) };
        extension.initialize(&ExtensionContext::new(self.shared_state.clone()))?;

        self.loaded_extensions
            .write()
            .insert(id.to_string(), extension);
        Ok(())
    }
}
```

**Extension Crate** (published separately):

```rust
use vulcan_extension::{Extension, ExtensionMetadata};

pub struct RedisMemory { client: redis::Client }

impl Extension for RedisMemory {
    fn metadata(&self) -> &ExtensionMetadata { /* ... */ }
    fn initialize(&self, ctx: &ExtensionContext) -> Result<()> {
        ctx.register_memory_backend("redis", Arc::new(self.clone()));
        Ok(())
    }
}

// Required export — dynamic library entry point
#[no_mangle]
pub unsafe extern fn create_extension() -> *mut dyn Extension {
    Box::into_raw(Box::new(RedisMemory::new()))
}
```

### Strategy B: WebAssembly — Sandboxed

```rust
fn load_wasm(&self, id: &str, manifest: &Manifest) -> Result<()> {
    use wasmtime::*;

    let engine = Engine::default();
    let module = Module::from_file(&engine, &manifest.entry)?;

    let mut linker = Linker::new(&engine);

    // Provide safe host functions (capability-gated)
    linker.func_wrap("env", "log", |msg: String| {
        log::info!("[wasm ext {}]: {}", id, msg);
    })?;
    linker.func_wrap("env", "register_tool", ...)?;

    let mut store = Store::new(&engine, ());
    store.limiter(|_| &mut StoreLimiter {
        memory_size: 1024 * 1024 * 10, // 10 MB cap
        ..Default::default()
    });

    let instance = linker.instantiate(&mut store, &module)?;
    let init = instance.get_typed_func::<(), ()>(&mut store, "init")
        .ok_or_else(|| anyhow!("Missing init export"))?;

    init.call(&mut store, ())?;
    Ok(())
}
```

**WASM Extension** (Rust → WASM):

```rust
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn init() {
    register_tool("weather", WeatherTool {});
}
```

### Strategy C: Scripting (JavaScript / Python)

```rust
fn load_javascript(&self, id: &str, manifest: &Manifest) -> Result<()> {
    use deno_core::*;

    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: vec![Extension::builder()
            .ops(vec![op_register_tool::<NotNeeded>])
            .build()],
        ..Default::default()
    });

    let script = std::fs::read_to_string(&manifest.entry)?;
    runtime.execute_script("<extension>", script)?;
    Ok(())
}
```

---

## 5. Security Model

### Defense in Depth

1. **Signature Verification**

```rust
fn verify_signature(&self, data: &[u8], sig_b64: &str) -> Result<()> {
    let sig_bytes = base64::decode(sig_b64)?;
    for key in &self.trusted_keys {
        if key.verify(data, &sig_bytes).is_ok() {
            return Ok(());
        }
    }
    anyhow::bail!("Invalid or untrusted signature");
}
```

2. **Permission Manifest**

```toml
[permissions]
filesystem = "read-only"   # none | read-only | read-write
network     = "none"       # none | allowed
environment = false        # allow env var access
memory_limit = "64MB"
```

3. **Sandboxing**
   - **WASM**: Full memory isolation; only imported host functions are available.
   - **Native**: Run untrusted extensions in a child process with seccomp-bpf (Linux) or sandbox-exec (macOS).
   - **Scripts**: Restricted globals; deny `fs`, `child_process`, etc.

4. **Resource Limits** (WASM)
   - Max memory: 10–64 MB
   - Max table elements: 1,000
   - Max execution time: timeouts via async interrupts

---

## 6. Extension Store API

### CLI Commands

```bash
vulcan extension list                    # List installed extensions
vulcan extension search <query>          # Search repositories
vulcan extension info <id>               # Show metadata & capabilities
vulcan extension install <id>            # Download + install
vulcan extension uninstall <id>          # Remove
vulcan extension update <id>             # Update to latest
vulcan extension verify <id>             # Verify signature
vulcan extension enable <id>             # Load into runtime
vulcan extension disable <id>            # Unload (if safe)
```

### In-Agent Commands

```text
!extensions list           # Show loaded extensions
!extensions enable <id>    # Load extension
!extensions disable <id>   # Unload extension
!extensions info <id>      # Show metadata
```

---

## 7. Repository Infrastructure

### Index Format (`index.json`)

Hosted at `https://repo.vulcan.dev/index.json`:

```json
{
  "version": "1.0",
  "name": "Official Vulcan Repository",
  "last_updated": "2024-06-01T12:00:00Z",
  "extensions": [
    {
      "id": "memory@redis",
      "version": "1.0.0",
      "download_url": "https://repo.vulcan.dev/packages/memory_redis-1.0.0.vpk",
      "checksum": "sha256:abc123...",
      "signature": "base64:sig...",
      "min_vulcan_version": "0.4.0",
      "tags": ["memory", "redis", "storage", "production"],
      "categories": ["memory-backend"],
      "recommended_for": ["backend", "web", "data-pipelines"],
      "ranking": {
        "downloads_last_30d": 3124,
        "avg_rating": 4.7,
        "usage_score": 0.89,
        "trend": "up"
      }
    }
  ]
}
```

### Tagging & Discoverability

Extensions can be tagged and categorized to enable powerful discovery and recommendation features.

| Field | Type | Description |
|-------|------|-------------|
| `tags` | `string[]` | Free-form tags (e.g. `memory`, `redis`, `storage`, `production`) |
| `categories` | `string[]` | Canonical category buckets (e.g. `memory-backend`, `tool-provider`, `event-handler`) |
| `recommended_for` | `string[]` | Project archetypes (e.g. `backend`, `web`, `ml`, `data-pipelines`) |
| `ranking` | object | Analytics for sorting: downloads, rating, usage_score (0–1), trend (up/down/stable) |

### Filtering & Sorting API

Clients can query the repository index with filters and sorting:

```bash
# CLI examples
vulcan extension search --tag memory --tag production --sort downloads
vulcan extension list --category memory-backend --trend up --min-rating 4.0
```

Programmatic (JSON query parameters):

```
GET https://repo.vulcan.dev/index.json?tags=memory,storage&category=memory-backend&sort=-ranking.downloads_last_30d
```

- Prefix `-` for descending (e.g. `-ranking.usage_score`)
- Multiple tags match as OR by default, AND with `&match=all`

### Personalized Recommendations

The store maintains anonymized, opt-in usage telemetry to generate recommendations:

- **Installed extensions**: Correlate with extensions used by similar projects.
- **Tool invocation patterns**: Suggest extensions that provide commonly invoked tools (e.g. users who often call `postgres_query` install `postgres-inspector`).
- **Session outcomes**: Recommend extensions that correlate with successful task completion.
- **Project context**: Language, framework, and manifest files (`Cargo.toml`, `package.json`, `docker-compose.yml`) drive archetype-based picks.

A local `recommendations.json` is cached in the agent config:

```json
{
  "generated_at": "2024-06-01T12:00:00Z",
  "project_archetype": "backend-rust-axum",
  "recommendations": [
    {
      "id": "memory@redis",
      "reason": "High usage among Rust backend projects; often paired with postgres query tools",
      "score": 0.92
    },
    {
      "id": "tool@postgres-inspector",
      "reason": "Complements existing postgres usage patterns",
      "score": 0.87
    }
  ]
}
```

### Trending & Popularity Signals

The ranking object supports sortable, comparable metrics:

- `downloads_last_30d` — Absolute popularity.
- `avg_rating` — Community rating (1–5).
- `usage_score` — Normalized engagement (0–1): fraction of installs that actively use the extension.
- `trend` — Direction: `up`, `down`, `stable`.

### Example: Finding Extensions

```bash
# Discover trending storage extensions
vulcan extension search --category storage --trend up --sort downloads

# Find recommended extensions for current project
vulcan extension recommend
```

### Package Format (`.vpk`)

A gzipped tarball:

```bash
tar czf memory_redis-1.0.0.vpk -C build/extension .
```

### Signing Workflow

1. Developer generates keypair:  
   `vulcan keygen` → `$HOME/.vulcan/keys/`
2. Publishes public key to GitHub / website / repo metadata.
3. Signs releases:  
   `vulcan sign memory_redis-1.0.0.vpk`
4. Repository index includes the signature.
5. Clients verify signature **before** installation.

---

## 8. Integration with Vulcan Internals

### Extension Context

```rust
pub struct ExtensionContext {
    pub agent_sender: mpsc::Sender<AgentCommand>,
    pub tool_registry: Arc<ToolRegistry>,
    pub memory_backends: Arc<RwLock<HashMap<String, Arc<dyn MemoryBackend>>>>,
    pub event_bus: EventBus,
}

impl ExtensionContext {
    pub fn register_tool(&self, name: String, tool: Arc<dyn Tool>) {
        self.tool_registry.register(name, tool);
    }

    pub fn register_memory_backend(&self, name: &str, backend: Arc<dyn MemoryBackend>) {
        self.memory_backends.write().insert(name.to_string(), backend);
    }

    pub fn register_event_handler<H>(&self, handler: H)
    where
        H: EventHandler + Send + Sync + 'static,
    {
        self.event_bus.subscribe(Arc::new(handler));
    }
}
```

### Event Hooks

```rust
pub enum Event {
    AgentStart { agent_id: String },
    AgentStop { agent_id: String },
    BeforeToolCall { tool: String, args: Value },
    AfterToolCall { tool: String, result: Result<Value> },
    MemoryQuery { session_id: String, query: String },
}
```

Extensions can subscribe to react to events:

```rust
impl Extension for MyLogger {
    fn initialize(&self, ctx: &ExtensionContext) -> Result<()> {
        ctx.register_event_handler(MyLogger {});
        Ok(())
    }
}

impl EventHandler for MyLogger {
    fn handle_event(&self, event: &Event) {
        match event {
            Event::BeforeToolCall { tool, .. } => log::info!("Calling {}", tool),
            _ => {}
        }
    }
}
```

---

## 9. Trust & Discovery

### Trust Model

- **TOFU** (Trust On First Use) for new repositories.
- Extensions **must be signed** by a trusted key.
- Official repository curated and scanned automatically.
- Users can add custom repositories but see clear warnings.

```rust
pub struct ExtensionStore {
    trusted_extensions: HashSet<String>, // IDs
    trusted_signers: HashSet<PublicKey>,
}
```

### Discovery Sources

- Official: `https://repo.vulcan.dev/index.json`
- Community: added via CLI
  ```bash
  vulcan repo add https://community.example.com/index.json
  ```

---

## 10. Example: Redis Memory Extension

**`Cargo.toml` (extension crate)**

```toml
[package]
name = "vulcan-redis-memory"
version = "0.1.0"

[lib]
crate-type = ["cdylib"]

[dependencies]
vulcan-extension = { path = "../../crates/extension" }
redis = "0.23"
```

**`src/lib.rs`**

```rust
use vulcan_extension::{Extension, ExtensionMetadata, ExtensionContext};
use redis::Client;

pub struct RedisMemoryBackend {
    client: Client,
}

impl RedisMemoryBackend {
    pub fn new() -> Self {
        let client = redis::Client::open("redis://127.0.0.1/").unwrap();
        Self { client }
    }
}

impl Extension for RedisMemoryBackend {
    fn metadata(&self) -> &ExtensionMetadata {
        use vulcan_extension::Version;
        static META: ExtensionMetadata = ExtensionMetadata {
            id: "memory@redis".into(),
            name: "Redis Memory Backend".into(),
            version: Version::new(1, 0, 0),
            description: "Store agent memory in Redis".into(),
            author: "Community".into(),
            license: "MIT".into(),
            repository: None,
            signature: None,
        };
        &META
    }

    fn initialize(&self, ctx: &ExtensionContext) -> Result<()> {
        ctx.register_memory_backend("redis", Arc::new(self.clone()));
        Ok(())
    }

    fn capabilities(&self) -> &[Capability] {
        static CAPS: &[Capability] = &[Capability::MemoryBackend("redis".into())];
        CAPS
    }
}

#[no_mangle]
pub unsafe extern fn create_extension() -> *mut dyn Extension {
    Box::into_raw(Box::new(RedisMemoryBackend::new()))
}
```

**Install & Use**

```bash
vulcan extension search redis
vulcan extension install memory@redis
vulcan extension enable memory@redis
```

Vulcan now uses Redis for memory storage when configured.

---

## Summary of Key Components

| Component | Purpose |
|----------|---------|
| **Extension Trait** | Common interface for all extensions |
| **Package Format (.vpk)** | Bundled manifest + binary/assets |
| **Repository Index** | JSON catalog of available extensions |
| **Signature Verification** | GPG/Ed25519 signing & trust |
| **Runtime Loaders** | Native (dylib) / WASM / Script (JS/Py) |
| **Sandboxing** | Capability-based permissions & isolation |
| **Extension Context** | Safe APIs for integration |
| **CLI / Agent Commands** | Discover, install, manage extensions |

This design provides a **secure, extensible plugin ecosystem** similar to VS Code or Deno: extensions are verified, sandboxed, and easy to distribute, while Vulcan retains full control over what they can do.
