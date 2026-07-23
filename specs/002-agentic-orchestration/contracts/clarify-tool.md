# Contract: clarify Tool

**Feature**: 002-agentic-orchestration

## Tool Identity

- **Name**: `clarify`
- **Toolset**: `clarify`

## Description

Ask the user a structured question when genuine ambiguity blocks progress.
Presents clear options (multiple-choice or open-ended) rather than guessing
silently. Reserved for decisions where the wrong choice has significant
downstream cost. Not for simple yes/no confirmation.

## Parameters Schema

```json
{
  "type": "object",
  "properties": {
    "question": {
      "type": "string",
      "description": "The question itself, and ONLY the question. Do NOT embed answer options here."
    },
    "choices": {
      "type": "array",
      "items": { "type": "string" },
      "description": "Up to 4 distinct, mutually exclusive options. The UI renders these as selectable rows. Omit entirely for a genuinely open-ended free-text question.",
      "maxItems": 4
    }
  },
  "required": ["question"]
}
```

## Return Contract

Returns the user's response as text:

- For multiple-choice: the selected option's text
- For open-ended: the user's free-form input

The tool blocks (awaits user response) until the user answers. In
non-interactive sessions (cron, oneshot), the tool returns an error:
`{"error": "Clarification requested but session is non-interactive."}`

## Interactive Behavior

- The CLI/TUI renders the question and choices as a pickable list
- The user selects one option or types a custom answer (5th "Other" option)
- The response is fed back to the agent as the tool result
- The agent incorporates the answer and continues

## Constraints

- Only available in interactive sessions (`ToolContext::interactive() == true`)
- Maximum 4 choices (the UI auto-appends an "Other" option for custom input)
- The tool is a blocking call — the turn loop pauses until the user responds
- Choices should be mutually exclusive and cover the realistic option space
