---
description: Custom TUI logger
---

# TuiLogger

Custom `log::Log` implementation that buffers log entries for TUI display.

- Entries stored in a global `OnceLock<Mutex<Vec<LogEntry>>>`.
- `TuiLogger::drain()` returns and clears all buffered entries.
- CLI TUI render loop drains entries each frame and appends to output area.
- Log level priority: `RUST_LOG` env var > `tuning.log_level` config > `info`.

## LogEntry

Structured entry with `level` (Error/Warn/Info/Debug/Trace) and `message`.
Implements `Display` as `[LEVEL] message`.
