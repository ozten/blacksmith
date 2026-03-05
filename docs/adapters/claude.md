# Claude Adapter

The default adapter. Parses Claude Code's `stream-json` JSONL format.

> [!WARNING]
> **Subscription vs API keys:** If `ANTHROPIC_API_KEY` is set in your environment, Claude Code will use API keys and bill per token — this can get expensive with automated loops. To use your Claude Code subscription instead, run `unset ANTHROPIC_API_KEY` before launching blacksmith.

## Configuration

```toml
[agent]
command = "claude"
args = ["-p", "{prompt}", "--dangerously-skip-permissions", "--verbose", "--output-format", "stream-json"]
```

## Key Flags

| Flag | Purpose |
|---|---|
| `-p {prompt}` | Non-interactive mode; reads prompt as argument and exits when done |
| `--output-format stream-json` | Emit JSONL events to stdout (required for metric extraction) |
| `--verbose` | Include tool-use detail in the JSONL stream |
| `--dangerously-skip-permissions` | Skip all permission prompts (required for headless use) |
| `--allowedTools "Bash Read Write Edit Glob Grep"` | Optional tool whitelist (alternative to skip-permissions) |
| `--model <model-id>` | Override the default model |
| `--max-turns <N>` | Cap the number of agentic turns |

## Built-in Metrics

| Metric | Description |
|---|---|
| `turns.total` | Total conversation turns |
| `turns.narration_only` | Turns with no tool use |
| `turns.parallel` | Turns with parallel tool calls |
| `turns.tool_calls` | Total tool call count |
| `cost.input_tokens` | Input token count |
| `cost.output_tokens` | Output token count |
| `cost.estimate_usd` | Session cost from result event |

## Extraction Rule Sources

| Source | Maps to |
|---|---|
| `tool_commands` | Tool use `input.command` fields |
| `text` | Assistant text blocks |

## Related

- [Adapters Overview](overview.md) — All adapters compared
- [Getting Started](../getting-started.md) — Default Claude setup
