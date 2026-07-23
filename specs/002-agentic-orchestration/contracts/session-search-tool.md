# Contract: session_search Tool

**Feature**: 002-agentic-orchestration

## Tool Identity

- **Name**: `session_search`
- **Toolset**: `session_search`

## Description

Search past conversation history stored in the session database. Returns
matching messages ranked by FTS5 relevance, with snippets and timestamps.
Use to recall decisions, prior solutions, and context from earlier
interactions without re-asking the user.

## Parameters Schema

```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "Search query. Supports FTS5 syntax: quoted phrases, boolean (AND/OR/NOT), prefix wildcards (deploy*). Required."
    },
    "limit": {
      "type": "integer",
      "description": "Max results to return. Default: 5, max: 20.",
      "default": 5
    },
    "session_id": {
      "type": "string",
      "description": "If provided with around_message_id, retrieve a window of messages around that message in the specified session."
    },
    "around_message_id": {
      "type": "integer",
      "description": "Message ID to center the window on (used with session_id for scroll/context retrieval)."
    },
    "window": {
      "type": "integer",
      "description": "Number of messages on each side of around_message_id. Default: 5, max: 20.",
      "default": 5
    }
  },
  "required": ["query"]
}
```

## Return Contract

### Search mode (query only)

Returns structured text:

```
Found 3 matching sessions:

[1] Session: abc123def | Role: user | Message ID: 42
    Snippet: ...we decided to use pytest with xdist for parallel test...

[2] Session: xyz789 | Role: assistant | Message ID: 107
    Snippet: ...the fix involved updating the config parser to handle...
```

### Scroll mode (session_id + around_message_id)

Returns a window of messages:

```
Session: abc123def (messages 37-47 of 152)

[37] user: What test framework should we use?
[38] assistant: I recommend pytest with xdist for parallel execution...
[39] user: Sounds good, let's go with that
...
[42] user (MATCH): we decided to use pytest with xdist for parallel test...
...
[47] assistant: Great, I'll set up the conftest.py file
```

## Performance

- FTS5 search over 100+ sessions: <1 second (SC-005)
- Uses the existing `messages_fts` virtual table and `SessionDb::search()`
- No external search index or embedding model required

## Degradation Behavior

- If FTS5 is unavailable (`fts_enabled == false`), returns:
  `{"error": "Session search is not available: full-text search index is not enabled."}`
- If the session store is locked or corrupt, returns:
  `{"error": "Session search failed: <detail>"}`
- Never crashes the turn loop on store errors.

## Configuration Keys

| Key | Default | Description |
|-----|---------|-------------|
| `session_search.max_results` | 20 | Hard limit on results returned |
| `session_search.max_window` | 20 | Max messages per scroll window |
