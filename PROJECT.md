# Project notes

Traceboard probes 1–25 configured HTTP(S) targets concurrently with a ten-second request timeout, a bounded interval and up to three redirects. Configuration is read through a 128 KiB cap; each target URL is limited to 2048 bytes. The latest 120 checks per URL remain in the existing SQLite checks table. The first check establishes a baseline; failure_threshold retains its existing health policy. No response bodies are saved. The localhost dashboard and manual refresh preserve English/Russian, System/Light/Dark, custom controls and thin themed scrollbars.

## Persistence and notifications

A dedicated OS thread owns SQLite; a 32-slot work channel keeps blocking SQL off Tokio. A single check and its state-change outbox intent commit together before monitor history is published. Failed storage leaves the published history unchanged. Telegram configuration is optional; when disabled, new checks do not create alert intents. Existing pending events remain paused. No additional service or database is required.

The outbox has at most 256 retained records. Known delivered/failed history is trimmed to 64 on enqueue and pruned first at capacity. Pending, sending and uncertain entries are never automatically discarded. If all slots are occupied, the check commits and a persistent overflow counter increments instead of silently claiming delivery. This is an explicit bounded overflow policy, not a promise to deliver every transition under unlimited load. Counters returned by /api/notifications are a cached snapshot; delivered/failed are retained records, not lifetime totals.

A separate sender claims FIFO work in a transaction, persists sending/attempt number, then performs one bounded network request. Starts are separated by at least one second. A response body is limited to 32 KiB; redirects are disabled; HTTP/API errors and bot tokens are never copied to the public overview or logs. The API must confirm ok=true and a positive message_id before delivery is confirmed. HTTP 200 with ok=false is not success.

Only a confirmed API 429 with retry_after between 1 and 3600 seconds permits retry, up to three total attempts. The head waits through its cooldown, preventing a recovery alert from overtaking its down alert. Other explicit 4xx rejections are terminal. Transport failure, malformed/oversized reply, 5xx or ambiguous response means uncertain, with no automatic resend. Restart changes interrupted sending rows to uncertain. Repeated completion of an old attempt cannot overwrite a newer state. This avoids blind duplicates; it does not claim exactly-once Telegram delivery.

Destination binding hashes the bot ID and chat, not the secret token. Token rotation for the same bot/chat keeps its queue; a different destination pauses old entries. The hash is an identity tag, not encryption. Tokens/chat credentials remain in process environment only, not notification rows. Alert text contains the configured target name, state, status and a stable Event # for diagnosis. The database remains private application data.

## Recovery and visibility

The delivery panel separates waiting, rejected, unconfirmed, destination-paused, overflow and storage/sender failure from service health. Stale status never becomes a false success. Check Telegram before acting on an unconfirmed event; there is deliberately no automatic resend control. Sender/storage errors require inspecting and preserving the database before restarting. Uncertain records are retained and consume capacity; a guided reconciliation/retention UI remains future work, not an implemented feature.

TRACEBOARD_TELEGRAM_API is solely a local test fixture override: only a literal loopback HTTP origin is accepted; production uses https://api.telegram.org. tests/outbox.py uses fake credentials and a local fake Bot API, never a real bot. CI also runs startup diagnostics, HTTP-rule regressions and Rust tests on the existing Windows/macOS/Linux matrix. Physical launchers, real Telegram credentials, power loss and screen-reader review are separate manual checks.

Bot API contract reference: https://core.telegram.org/bots/api#making-requests and https://core.telegram.org/bots/api#responseparameters.

## Recovery verification

Whole-second retry deadlines round upward by one second so restart cannot shorten a confirmed retry_after. The overview reports only whitelisted reason counts; an unknown saved reason contributes to other, without exposing its text. The fixed check-write stderr warning is emitted at most once per process; current failures remain visible in the overview. check_storage_error follows the most recent check write and is not cleared by unrelated sender polling; sender_stopped stays sticky until restart.

tests/outbox_recovery.py kills the real process during a held fake API request, restarts a persisted cooldown, rejects local delivery confirmation, verifies continued checks and no duplicate sending after restart, forces an atomic intent write failure, then restores storage. A locally seeded 256-record uncertain queue verifies actual overflow/history persistence without flooding any bot. Real credentials, hardware power loss and physical platform launchers remain manual checks.
