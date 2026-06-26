# Finding 02: Current Message Ordering Analysis

## Summary

When `aichat --file A.txt --file B.txt -- "what does this code do?"` is executed, the user's prompt text is placed **FIRST** in the assembled text field, and file document contents are appended **AFTER**. This ordering is determined at a single location: `src/config/input.rs:78-80`.

---

## (a) Code Snippets Showing the Assembly

### Step 1: CLI Argument Parsing (`src/cli.rs:119-120`)

The trailing arguments (the user's prompt text) are joined with spaces:

```rust
// src/cli.rs:119 (inside Cli::text() method)
false => {
    // ...
    let text = self.text.join(" ");  // e.g. "what does this code do?"
    if stdin_text.is_empty() {
        Ok(Some(text))
    } else {
        Ok(Some(format!("{text}\n{stdin_text}")))
    }
}
```

### Step 2: Entry Point (`src/main.rs:328-349`)

The `create_input()` function passes the text and file paths to `Input::from_files_with_spinner`:

```rust
// src/main.rs:328-349
async fn create_input(
    config: &GlobalConfig,
    text: Option<String>,
    file: &[String],
    abort_signal: AbortSignal,
) -> Result<Input> {
    let input = if file.is_empty() {
        Input::from_str(config, &text.unwrap_or_default(), None)
    } else {
        Input::from_files_with_spinner(
            config,
            &text.unwrap_or_default(),  // <-- prompt text passed as raw_text
            file.to_vec(),               // <-- file paths
            None,
            abort_signal,
        )
        .await?
    };
    // ...
}
```

### Step 3: The Critical Assembly — `Input::from_files()` (`src/config/input.rs:57-117`)

This is **where ordering is determined**:

```rust
// src/config/input.rs:57-117
pub async fn from_files(
    config: &GlobalConfig,
    raw_text: &str,           // <-- the user's prompt, e.g. "what does this code do?"
    paths: Vec<String>,       // <-- file paths from --file flags
    role: Option<Role>,
) -> Result<Self> {
    let loaders = config.read().document_loaders.clone();
    let (raw_paths, local_paths, remote_urls, external_cmds, protocol_paths, with_last_reply) =
        resolve_paths(&loaders, paths)?;
    let mut last_reply = None;
    let (documents, medias, data_urls) = load_documents(
        &loaders, local_paths, remote_urls, external_cmds, protocol_paths,
    ).await.context("Failed to load files")?;
    
    // ╔══════════════════════════════════════════════════════════════╗
    // ║  THIS IS WHERE ORDERING IS DETERMINED (lines 77-101)        ║
    // ╚══════════════════════════════════════════════════════════════╝
    
    let mut texts = vec![];
    
    // >>> LINE 78-80: PROMPT TEXT IS PUSHED FIRST <<<
    if !raw_text.is_empty() {
        texts.push(raw_text.to_string());   // ← DYNAMIC prompt goes FIRST
    };
    
    // Lines 81-93: Handle "with_last_reply" (optional, from %% syntax)
    if with_last_reply {
        if let Some(LastMessage { input, output, .. }) = config.read().last_message.as_ref() {
            // ... pushes last reply text ...
            if let Some(v) = last_reply.clone() {
                texts.push(format!("\n{v}"));
            }
        }
    }
    
    // >>> LINES 94-101: FILE DOCUMENTS ARE PUSHED AFTER <<<
    let documents_len = documents.len();
    for (kind, path, contents) in documents {
        if documents_len == 1 && raw_text.is_empty() {
            texts.push(format!("\n{contents}"));
        } else {
            texts.push(format!(
                "\n============ {kind}: {path} ============\n{contents}"
            ));
        }
    }
    
    // >>> LINE 105: ALL PARTS JOINED INTO SINGLE STRING <<<
    Ok(Self {
        // ...
        text: texts.join("\n"),  // ← Final assembled text
        // ...
    })
}
```

### Step 4: Message Content Creation (`src/config/input.rs:341-355`)

The assembled text becomes the message content:

```rust
// src/config/input.rs:341-355
pub fn message_content(&self) -> MessageContent {
    if self.medias.is_empty() {
        MessageContent::Text(self.text())  // ← The full assembled text as a single string
    } else {
        let mut list: Vec<MessageContentPart> = self.medias.iter()
            .cloned()
            .map(|url| MessageContentPart::ImageUrl { image_url: ImageUrl { url } })
            .collect();
        if !self.text.is_empty() {
            list.insert(0, MessageContentPart::Text { text: self.text() });
        }
        MessageContent::Array(list)
    }
}
```

### Step 5: Messages Array Construction (`src/config/role.rs:225-251`)

The message content is placed in the user message at the end of the messages array:

```rust
// src/config/role.rs:225-251
pub fn build_messages(&self, input: &Input) -> Vec<Message> {
    let mut content = input.message_content();
    let mut messages = if self.is_empty_prompt() {
        vec![Message::new(MessageRole::User, content)]
    } else if self.is_embedded_prompt() {
        content.merge_prompt(|v: &str| self.prompt.replace(INPUT_PLACEHOLDER, v));
        vec![Message::new(MessageRole::User, content)]
    } else {
        let mut messages = vec![];
        let (system, cases) = parse_structure_prompt(&self.prompt);
        if !system.is_empty() {
            messages.push(Message::new(
                MessageRole::System,
                MessageContent::Text(system.to_string()),
            ));
        }
        // ... few-shot examples ...
        messages.push(Message::new(MessageRole::User, content));  // ← User message is LAST
        messages
    };
    // ...
    messages
}
```

---

## (b) Concrete Example of What the Message Looks Like

### Command

```bash
aichat --file src/main.rs --file src/lib.rs -- "what does this code do?"
```

### Resulting `text` Field (the content of the user message)

```
what does this code do?

============ FILE: src/main.rs ============
fn main() {
    println!("Hello, world!");
}

============ FILE: src/lib.rs ============
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}
```

### Breakdown of `texts` Vector Before Join

| Index | Source | Content |
|-------|--------|---------|
| 0 | `raw_text` (line 79) | `"what does this code do?"` |
| 1 | Document loop (line 97-99) | `"\n============ FILE: src/main.rs ============\nfn main() {\n    println!(\"Hello, world!\");\n}"` |
| 2 | Document loop (line 97-99) | `"\n============ FILE: src/lib.rs ============\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}"` |

After `texts.join("\n")`:
```
what does this code do?\n\n============ FILE: src/main.rs ============\n...\n\n============ FILE: src/lib.rs ============\n...
```

### Final API Request Body (e.g., OpenAI format)

```json
{
  "model": "gpt-4",
  "messages": [
    {
      "role": "user",
      "content": "what does this code do?\n\n============ FILE: src/main.rs ============\nfn main() {\n    println!(\"Hello, world!\");\n}\n\n============ FILE: src/lib.rs ============\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}"
    }
  ]
}
```

### Special Case: Single File Without Prompt Text

```bash
aichat --file src/main.rs
```

When `documents_len == 1 && raw_text.is_empty()`, the document header is omitted:

```json
{
  "role": "user",
  "content": "\nfn main() {\n    println!(\"Hello, world!\");\n}"
}
```

### With System Prompt (role has a prompt configured)

```bash
aichat --role code-reviewer --file src/main.rs -- "review this"
```

```json
{
  "messages": [
    {
      "role": "system",
      "content": "You are a code reviewer..."
    },
    {
      "role": "user",
      "content": "review this\n\n============ FILE: src/main.rs ============\nfn main() {\n    println!(\"Hello, world!\");\n}"
    }
  ]
}
```

---

## (c) Identification of Where Ordering Is Determined

### The Single Decision Point

**File**: `src/config/input.rs`  
**Lines**: 78-80 (prompt first) and 94-101 (documents after)  
**Function**: `Input::from_files()`

The ordering is determined by the **sequential push operations** into the `texts` vector:

1. **Line 78-80** — `texts.push(raw_text.to_string())` — pushes the user's prompt FIRST
2. **Lines 94-101** — `for (kind, path, contents) in documents { texts.push(...) }` — pushes file contents AFTER

### Why This Is the Only Decision Point

- The `texts.join("\n")` at line 105 creates a single flat string
- This string is stored as `self.text` and never reordered afterward
- `message_content()` wraps it directly as `MessageContent::Text(self.text())`
- `build_messages()` places it into the user message without modification
- Provider-specific body builders serialize it as-is into the JSON content field

### To Change Ordering

To make files come FIRST (for prefix caching optimization), you would:

1. Move lines 94-101 (the document loop) BEFORE lines 78-80 (the raw_text push)
2. Or equivalently: push `raw_text` AFTER the document loop instead of before it

The change is localized to **a single 5-line block** within `Input::from_files()`.

### Current Order (problematic for caching):
```
[DYNAMIC prompt] → [STATIC file 1] → [STATIC file 2] → ...
```

### Desired Order (cache-friendly):
```
[STATIC file 1] → [STATIC file 2] → ... → [DYNAMIC prompt]
```

---

## Flow Diagram

```
CLI args: "what does this code do?"          --file src/main.rs --file src/lib.rs
         │                                    │
         ▼                                    ▼
    Cli::text()                          cli.file vec
    joins with " "                       ["src/main.rs", "src/lib.rs"]
         │                                    │
         ▼                                    ▼
    ┌─────────────────────────────────────────────┐
    │         Input::from_files()                   │
    │                                               │
    │  texts = []                                   │
    │                                               │
    │  ┌─ Step 1 (line 78-80): ──────────────────┐ │
    │  │ texts.push("what does this code do?")    │ │
    │  └──────────────────────────────────────────┘ │
    │                                               │
    │  ┌─ Step 2 (lines 94-101): ────────────────┐ │
    │  │ texts.push("FILE: src/main.rs\n...")     │ │
    │  │ texts.push("FILE: src/lib.rs\n...")      │ │
    │  └──────────────────────────────────────────┘ │
    │                                               │
    │  text = texts.join("\n")  ← FINAL ORDERING    │
    └─────────────────────────────────────────────────┘
         │
         ▼
    Input { text: "what does this code do?\n\n============..." }
         │
         ▼
    input.message_content() → MessageContent::Text(self.text())
         │
         ▼
    Role::build_messages() → [Message { role: User, content: ... }]
         │
         ▼
    openai_build_chat_completions_body() → JSON { "content": "..." }
```

---

## Key Takeaway

The entire user message content—prompt text plus all file contents—is assembled into a **single flat string** at `src/config/input.rs:77-105`. The ordering decision happens through the simple fact that `raw_text` is pushed to the `texts` vector at line 79, before the document contents loop at lines 94-101. Reversing these two code blocks would place static file content first (enabling prefix caching) and dynamic prompt text last.
