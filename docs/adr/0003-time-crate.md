# 0003 — `jiff` for time

Status: Accepted (2026-10-12)

## Context
okbot stores timestamps, applies retention and dedupe TTLs, and shows the current time in a user's
timezone inside prompts. `chrono` is widespread but its timezone support needs a separate database
crate; `time` lacks IANA timezones.

## Decision
Use `jiff`: `jiff::Timestamp` for stored instants (UTC), `jiff::Zoned` only when formatting for a user.

## Consequences
Platform timestamps (Zalo uses milliseconds since the epoch) are converted at the adapter edge.
