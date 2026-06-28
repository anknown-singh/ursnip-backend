# Desktop Architecture

## Tauri + Rust Desktop App

The desktop application is the heart of Keystone — it monitors all keyboard input system-wide and injects expanded snippets into any application.

---

## Architecture Layers

```
┌─────────────────────────────────────────────────────────────┐
│                 React GUI Layer                             │
│  Settings Window  │  Snippet Editor  │  System Tray Menu   │
└────────────┬──────────────────────────┬────────────────────┘
             │ IPC Commands              │
             │ (Tauri invoke)            │
             ▼                           ▼
┌─────────────────────────────────────────────────────────────┐
│           Rust Backend (Tokio Runtime)                      │
│                                                              │
│  ┌─ Keyboard Hook Module                                   │
│  │  ├─ raw_input.rs (WM_INPUT handler)                    │
│  │  └─ buffer.rs (keystroke buffer + matching)            │
│  │                                                          │
│  ├─ Snippet Engine                                         │
│  │  ├─ Template parser (SDK)                              │
│  │  ├─ Variable resolver                                  │
│  │  ├─ Text injector (enigo + clipboard)                  │
│  │  └─ AI client (HTTP)                                   │
│  │                                                          │
│  ├─ Storage                                                │
│  │  └─ LocalStore (SQLite)                                │
│  │                                                          │
│  ├─ Sync Manager                                           │
│  │  ├─ Realtime WebSocket (Supabase)                      │
│  │  └─ Polling task (REST API)                            │
│  │                                                          │
│  ├─ Auth Manager                                           │
│  │  └─ Token storage (Windows Credential Manager)         │
│  │                                                          │
│  └─ Tauri Commands                                         │
│     ├─ snippet commands                                    │
│     ├─ auth commands                                       │
│     ├─ settings commands                                   │
│     └─ sync commands                                       │
└─────────────────────────────────────────────────────────────┘
         │ Database              │ WebSocket        │ HTTP
         │ (SQLite)              │ (Supabase)       │ (NestJS)
         ▼                       ▼                  ▼
    ┌────────────┬──────────────────────┬────────────────┐
    │  SQLite    │  Supabase Realtime   │  NestJS API    │
    └────────────┴──────────────────────┴────────────────┘
```

---

## Core Modules

### 1. Keyboard Hook (`hook/mod.rs` + `hook/raw_input.rs`)

**Purpose**: Monitor all keystrokes system-wide

**Implementation**: Windows Raw Input (not SetWindowsHookEx)

```rust
// Pseudo-code flow
fn wnd_proc(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) {
    match msg {
        WM_INPUT => {
            // Get raw input data
            GetRawInputData() -> RID_INPUT
            
            // Get virtual key + scan code
            let vk = ri_keyboard.VKey
            
            // Convert to Unicode character
            ToUnicode(vk) -> Some(char)
            
            // Send to keystroke buffer
            keystroke_buffer.push(char)
        }
        _ => DefWindowProc(...)
    }
}
```

**Why Raw Input?**
- No DLL injection (unlike SetWindowsHookEx WH_KEYBOARD_LL)
- Works with antivirus/EDR (no blocked)
- Uses message-only window (HWND_MESSAGE)
- Less overhead, better performance

---

### 2. Keystroke Buffer (`hook/buffer.rs`)

**Purpose**: Match triggers in a rolling buffer

**Data Structure**:
```rust
pub struct KeystrokeBuffer {
    buffer: VecDeque<char>,  // Max 200 chars
    max_size: usize,
}
```

**Matching Algorithm**:
```rust
fn check_for_trigger(buffer: &VecDeque<char>) -> Option<Snippet> {
    // Look for '/' in buffer (triggers start with /)
    if let Some(slash_pos) = buffer.iter().rposition(|&c| c == '/') {
        let candidate: String = buffer
            .iter()
            .skip(slash_pos)
            .collect();
        
        // O(1) lookup
        if let Some(snippet) = TRIGGER_INDEX.get(&candidate) {
            return Some(snippet.clone());
        }
    }
    None
}
```

**Performance**: O(buffer_len + 1) per keystroke ≈ sub-microsecond

---

### 3. Snippet Engine (`engine/mod.rs`)

**Purpose**: Orchestrate snippet expansion

```rust
pub struct SnippetEngine {
    local_store: LocalStore,
    ai_client: AIClient,
    trigger_index: HashMap<String, Snippet>,
}

impl SnippetEngine {
    pub async fn expand_and_inject(
        &mut self,
        trigger: String,
        context: Option<String>,
    ) -> Result<()> {
        // 1. Get snippet from local store
        let snippet = self.local_store.get_snippet(&trigger)?;
        
        // 2. Determine type
        match snippet.snippet_type {
            SnippetType::Static => {
                // Simple text, no processing needed
                let content = snippet.content;
                self.inject(trigger.len(), &content).await?;
            }
            SnippetType::Dynamic => {
                // Parse + render variables
                let ast = parse_template(&snippet.content)?;
                let rendered = render_template(&ast, &self.context)?;
                self.inject(trigger.len(), &rendered).await?;
            }
            SnippetType::AI => {
                // Call AI service
                let result = self.ai_client.expand(
                    &trigger,
                    &snippet.system_prompt,
                    context,
                ).await?;
                
                // Expand the trigger + inject result
                self.inject(trigger.len(), &result).await?;
            }
        }
        
        Ok(())
    }
}
```

---

### 4. Text Injector (`engine/injector.rs`)

**Purpose**: Inject expanded text into focused application

**Two Strategies**:

#### Strategy A: Backspace + Type (≤100 chars)
```rust
impl TextInjector {
    pub async fn inject_small(&mut self, trigger_len: usize, text: &str) {
        // Delete trigger (backspace)
        for _ in 0..trigger_len {
            self.enigo.key(Key::Backspace).unwrap();
        }
        
        sleep(Duration::from_millis(50)).await;
        
        // Type expansion
        self.enigo.text(text).unwrap();
    }
}
```

**Pros**: Simple, no clipboard pollution
**Cons**: Slow for long text, per-char events in some apps

#### Strategy B: Clipboard + Paste (>100 chars)
```rust
pub async fn inject_large(&mut self, trigger_len: usize, text: &str) {
    // Save current clipboard
    let original_clipboard = self.clipboard.read().unwrap();
    
    // Delete trigger
    for _ in 0..trigger_len {
        self.enigo.key(Key::Backspace).unwrap();
    }
    
    sleep(Duration::from_millis(50)).await;
    
    // Set clipboard to expansion
    self.clipboard.write(text.to_string()).unwrap();
    
    // Paste
    self.enigo.hotkey(Modifier::Control, Key::V).unwrap();
    
    // Restore clipboard after delay
    tokio::spawn(async move {
        sleep(Duration::from_millis(500)).await;
        self.clipboard.write(original_clipboard).unwrap();
    });
}
```

**Pros**: Fast for large text, efficient
**Cons**: Clipboard pollution (temporary), some apps paste differently

---

### 5. Local Storage (`store/mod.rs`)

**Purpose**: Cache snippets locally for fast matching and offline access

**Database**: SQLite with WAL mode

```rust
pub struct LocalStore {
    conn: Connection,  // SQLite
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredSnippet {
    pub id: String,
    pub user_id: String,
    pub trigger: String,
    pub content: String,
    pub snippet_type: SnippetType,
    pub folder_id: Option<String>,
    pub version: u64,  // For sync
    pub created_at: DateTime,
    pub updated_at: DateTime,
    pub deleted_at: Option<DateTime>,  // Soft delete
}

impl LocalStore {
    pub fn load_all_snippets(&self) -> Result<Vec<StoredSnippet>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM snippets WHERE deleted_at IS NULL"
        )?;
        let snippets = stmt.query_map([], |row| {
            Ok(StoredSnippet {
                id: row.get(0)?,
                user_id: row.get(1)?,
                trigger: row.get(2)?,
                // ... etc
            })
        })?;
        Ok(snippets.collect::<Result<Vec<_>, _>>()?)
    }
    
    pub fn upsert_snippet(&self, snippet: StoredSnippet) -> Result<()> {
        self.conn.execute(
            "INSERT INTO snippets (...) VALUES (...)
             ON CONFLICT(id) DO UPDATE SET ...",
            params![...],
        )?;
        Ok(())
    }
}
```

**Schema**:
```sql
CREATE TABLE snippets (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    trigger TEXT NOT NULL UNIQUE,
    content TEXT NOT NULL,
    snippet_type TEXT NOT NULL,
    folder_id TEXT,
    version INTEGER NOT NULL,
    created_at TIMESTAMP,
    updated_at TIMESTAMP,
    deleted_at TIMESTAMP NULL
);

CREATE INDEX idx_user_snippets ON snippets(user_id);
CREATE INDEX idx_trigger ON snippets(trigger);
```

---

### 6. Sync Manager (`sync/mod.rs` + `sync/realtime.rs`)

**Purpose**: Keep local snippets in sync with cloud

**Two Parallel Paths**:

#### Path 1: Realtime WebSocket
```rust
pub struct RealtimeManager {
    url: String,
    jwt: String,
}

impl RealtimeManager {
    pub async fn run(&mut self) {
        loop {
            match self.connect().await {
                Ok(ws) => {
                    // Subscribe to postgres_changes
                    let msg = json!({
                        "type": "subscribe",
                        "channel": "snippets_changes",
                        "postgres_changes": [{
                            "event": "*",
                            "schema": "public",
                            "table": "snippets",
                            "filter": format!("user_id=eq.{}", self.user_id)
                        }]
                    });
                    
                    ws.send(msg).await?;
                    
                    // Listen for changes
                    while let Some(msg) = ws.next().await {
                        match msg? {
                            postgres_changes => {
                                // Update local store
                                self.local_store.upsert(payload)?;
                                // Rebuild trigger index
                                self.rebuild_index()?;
                            }
                        }
                    }
                }
                Err(e) => {
                    // Reconnect with backoff
                    sleep(exponential_backoff()).await;
                }
            }
        }
    }
}
```

#### Path 2: REST Polling Fallback
```rust
pub async fn polling_task(
    api_client: APIClient,
    local_store: LocalStore,
) {
    let mut last_version = local_store.get_last_version();
    
    loop {
        sleep(Duration::from_secs(30)).await;
        
        // Poll for changes since last_version
        match api_client.get_deltas(last_version).await {
            Ok(deltas) => {
                for delta in deltas {
                    local_store.apply_delta(delta)?;
                    last_version = delta.version;
                }
            }
            Err(_) => {
                // Network error, will retry
            }
        }
    }
}
```

---

### 7. Authentication (`auth/mod.rs`)

**Purpose**: Store and manage Clerk JWT token

```rust
pub struct AuthManager;

impl AuthManager {
    pub fn save_token(token: String) -> Result<()> {
        // Windows Credential Manager
        winapi::save_to_credential_manager(
            "Keystone",  // target
            token
        )
    }
    
    pub fn get_token() -> Result<String> {
        winapi::read_from_credential_manager("Keystone")
    }
    
    pub fn clear_token() -> Result<()> {
        winapi::delete_from_credential_manager("Keystone")
    }
}
```

**Security**: Credentials stored in Windows Credential Manager (encrypted at rest)

---

### 8. AI Client (`engine/ai_client.rs`)

**Purpose**: Call AI expansion endpoint

```rust
pub struct AIClient {
    http_client: reqwest::Client,
    api_url: String,
}

impl AIClient {
    pub async fn expand(
        &self,
        trigger: &str,
        system_prompt: &str,
        context: Option<String>,
    ) -> Result<String> {
        let payload = json!({
            "trigger": trigger,
            "system_prompt": system_prompt,
            "context": context,
        });
        
        let response = self.http_client
            .post(&format!("{}/ai/expand", self.api_url))
            .json(&payload)
            .send()
            .await?;
        
        let result: AIExpandResponse = response.json().await?;
        Ok(result.expanded_text)
    }
}
```

---

## Tauri Commands (IPC)

Frontend invokes Rust backend via Tauri commands:

```typescript
// React side
const snippet = await invoke('get_snippet', { trigger: '/addr' });

// Rust side
#[tauri::command]
fn get_snippet(trigger: String) -> Result<Snippet, String> {
    // Implementation
}
```

**Available Commands**:

### Snippets
- `list_snippets()` — Get all snippets
- `get_snippet(trigger)` — Get by trigger
- `create_snippet(trigger, content)` — Create
- `update_snippet(id, ...)` — Update
- `delete_snippet(id)` — Delete

### Auth
- `get_auth_status()` — Check logged in
- `save_token(token)` — Store JWT
- `clear_token()` — Logout
- `get_user_profile()` — User info

### Sync
- `trigger_sync()` — Force immediate sync
- `get_sync_status()` — Status info
- `pause_sync()` / `resume_sync()` — Control

### Settings
- `get_settings()` — App config
- `save_settings(settings)` — Update config
- `get_hotkey()` — Current hotkey
- `set_hotkey(key)` — Change hotkey

---

## Event Flow Example

```
User types "/addr" in Google Docs
    ↓
[OS Hook Thread]
WM_INPUT → ToUnicode('r') → keystroke_buffer.push('r')
    ↓
[Main Tokio Task]
buffer.rfind('/') finds "/addr"
trigger_index.get("/addr") → Some(Snippet)
    ↓
[Async Task]
SnippetEngine::expand_and_inject()
    ├─ LocalStore::get_snippet() → "123 Main St"
    ├─ render_template (no variables)
    └─ TextInjector::inject(5, "123 Main St")
        ├─ Enigo: 5× Backspace
        ├─ Sleep 50ms
        └─ Enigo: type "123 Main St"
    ↓
Text appears in Google Docs: "123 Main St"
```

---

## Development Setup

### Prerequisites
- Rust 1.75+
- Tauri CLI: `cargo install tauri-cli`
- Node.js 20+
- Windows SDK

### Commands
```bash
# Dev mode (hot reload)
cargo tauri dev

# Build
cargo tauri build

# Release build
cargo tauri build --release
```

---

## Performance Characteristics

| Operation | Target | Current |
|-----------|--------|---------|
| Keystroke → detection | <1ms | TBD |
| Trigger matching | <1ms | TBD |
| Template parsing | <5ms | TBD |
| Text injection | <50ms | TBD |
| Realtime latency | <2s | TBD |
| Memory usage (idle) | <50MB | TBD |
| CPU usage (idle) | <0.5% | TBD |

---

## Threading Model

- **OS Hook Thread**: Dedicated Windows hook thread (non-blocking)
- **Tauri Main Thread**: UI event loop
- **Tokio Runtime**: Async tasks (sync, networking, AI calls)
- **Sync Tasks**: Realtime + polling (background)

All communication via channels to avoid blocking.

---

## Future Enhancements

1. **macOS Support**: Native keyboard hook (different API)
2. **Linux Support**: X11/Wayland keyboard monitoring
3. **Plugin System**: Load .dll/.so plugins
4. **Advanced Automation**: Click, scroll, wait, conditional logic
5. **Voice Input**: Dictation + trigger
6. **Computer Vision**: Screenshot analysis for context
