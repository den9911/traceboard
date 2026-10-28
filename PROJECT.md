# Project notes

Traceboard reads up to 25 HTTP(S) targets from a local JSON file, probes them concurrently with a ten-second timeout and stores a rolling in-memory history. The localhost server exposes a read-only overview and a manual refresh endpoint. Overlapping refreshes are skipped. The browser polls for updates and stores its language and theme preferences locally.

Next milestones: verify the Rust build on all three CI platforms, persist checks with a retention policy, add opt-in Telegram alerts on state transitions, and capture real dashboard screenshots. This early version has no authentication and is intentionally bound to loopback; it is not suitable for exposing on a network.
