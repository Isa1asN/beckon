# Claude Code hook fixtures

Provenance matters here: these files are the contract the adapter is tested
against, so it should be obvious which ones reflect reality and which are
constructed.

**Captured from a live Claude Code 2.1.245 session** — field-for-field real:

- `stop.json`
- `session_end.json`
- `post_tool_use_failure.json`
- `post_tool_use_failure_interrupt.json` (real shape; `is_interrupt` flipped)

**Constructed from the documented schema, not yet observed in the wild.** These
events need an interactive session or an API failure to fire, neither of which a
`claude -p` run produces:

- `notification_*.json` — need an interactive permission prompt
- `stopfailure_*.json` — need a real API error (rate limit, overload)
- `subagent_stop.json`, `pre_compact.json`, `session_start.json`
- `user_prompt_submit.json`

To capture more, bind a dump-only hook and drive a real session:

```bash
claude --settings /path/to/dump-settings.json -p "..."
```

or set `BECKON_DUMP=/tmp/hooks.jsonl` with beckon already installed, which
appends every raw payload as one JSON line.
