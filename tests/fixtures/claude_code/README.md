# Claude Code hook fixtures

Provenance matters here: these files are the contract the adapter is tested
against, so it should be obvious which ones reflect reality and which are
constructed.

**Captured from a live Claude Code 2.1.245 session** — field-for-field real:

- `stop.json`
- `session_end.json`
- `post_tool_use_failure.json`
- `post_tool_use_failure_interrupt.json` (real shape; `is_interrupt` flipped)

**Read from the payload builder in Claude Code 2.1.288** — the field names are
what the shipped code writes, but no live payload was captured:

- `stopfailure_rate_limit.json`, `stopfailure_server_error.json` — the error
  kind is a plain string under `error`, the field the event's matcher filters on
- `user_prompt_submit_task_notification.json` — `UserPromptSubmit` also fires
  when a background task reports back; the payload has no origin field, only the
  prompt text. That text is copied from a real transcript's
  `<task-notification>` entry, and the hook firing at that moment was confirmed
  by a recorded turn-start timestamp matching it to the millisecond.

**Constructed from the documented schema, not yet observed in the wild.** These
events need an interactive session to fire, which a `claude -p` run does not
produce:

- `notification_*.json` — need an interactive permission prompt
- `stopfailure_unknown_shape.json` — deliberately not a real shape
- `subagent_stop.json`, `pre_compact.json`, `session_start.json`
- `user_prompt_submit.json`

To capture more, bind a dump-only hook and drive a real session:

```bash
claude --settings /path/to/dump-settings.json -p "..."
```

or set `BECKON_DUMP=/tmp/hooks.jsonl` with beckon already installed, which
appends every raw payload as one JSON line.
