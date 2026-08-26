# HyperCode + NeuroCode Integration Test Plan

## Test Scenarios

### 1. Basic HyperCode Functionality
- [ ] `/hypercode status` displays current configuration
- [ ] `/hypercode toggle` toggles enabled state and persists to config.yaml
- [ ] `/hypercode configure explorer anthropic claude-sonnet-4-20250514` saves model to config
- [ ] `/hypercode configure implementor anthropic --reasoning high` saves reasoning level
- [ ] `/hypercode configure explorer anthropic --tokens 128000` saves token limit
- [ ] `/hypercode configure implementor anthropic --turns 10` saves turn limit

### 2. Config Persistence
- [ ] After `/hypercode toggle`, check config.yaml contains `hypercode.enabled: true/false`
- [ ] After configure commands, check config.yaml contains provider-specific settings
- [ ] Config survives restart (reload config and verify settings persist)

### 3. TUI Integration
- [ ] TUI loads `hypercode_enabled` from config at startup
- [ ] TUI shows HyperCode badge when enabled
- [ ] Clicking badge in TUI toggles HyperCode state
- [ ] Status notices appear in TUI transcript

### 4. Additive with NeuroCode
- [ ] `/hypercode status` shows NeuroCode integration status when NeuroCode is enabled
- [ ] When both enabled, TUI startup shows both features active
- [ ] HyperCode status displays synergy information when NeuroCode active
- [ ] Each feature can be toggled independently
- [ ] Both features work together without conflict

### 5. CLI vs TUI Behavior
- [ ] CLI `/hypercode status` prints to stdout
- [ ] TUI `/hypercode status` displays in transcript as notices
- [ ] Both surfaces show identical information
- [ ] Configuration changes in CLI surface in TUI on reload
- [ ] Configuration changes in TUI surface in CLI on restart

### 6. Edge Cases
- [ ] Invalid provider handling
- [ ] Invalid reasoning level validation
- [ ] Invalid token count validation
- [ ] Invalid turn count validation
- [ ] Config file corrupted fallback

### 7. Multiple Providers
- [ ] Configure explorer for anthropic
- [ ] Configure explorer for openai
- [ ] Configure implementor for anthropic
- [ ] `/hypercode status` shows provider-specific settings
- [ ] Each provider maintains independent settings

## Test Commands

```bash
# Test 1: Basic toggle
joey config get hypercode.enabled  # Should be empty or false
echo "/hypercode toggle" | joey
joey config get hypercode.enabled  # Should be true
echo "/hypercode toggle" | joey
joey config get hypercode.enabled  # Should be false

# Test 2: Configure settings
echo "/hypercode configure explorer anthropic claude-sonnet-4-20250514" | joey
joey config get hypercode.explorer.anthropic.model  # Should be claude-sonnet-4-20250514

# Test 3: NeuroCode integration (if NeuroCode enabled)
echo "/neurocode enable" | joey  # First enable NeuroCode
echo "/hypercode status" | joey  # Should show NeuroCode synergy section

# Test 4: TUI startup
joey --tui  # Should load hypercode_enabled from config
```

## Expected Behavior

### config.yaml Structure
```yaml
hypercode:
  enabled: true
  explorer:
    anthropic:
      model: "claude-sonnet-4-20250514"
      max_tokens: 128000
      max_turns: 8
      reasoning_level: "high"
  implementor:
    anthropic:
      model: "gpt-4o-mini"
      max_tokens: 0
      max_turns: 12
      reasoning_level: "low"
```

### NeuroCode + HyperCode Synergy
When both are enabled:
- NeuroCode provides dependency-aware context to ALL agent turns
- HyperCode adds parallel task decomposition via batch delegation
- Each HyperCode subagent receives NeuroCode's context based on its task
- Both features work independently and additively
- Each can be toggled without affecting the other