# Project notes

Traceboard reads up to 25 HTTP(S) targets from a local JSON file, probes them concurrently with a ten-second timeout and stores the latest 120 checks per target in SQLite. The localhost server exposes a read-only overview and a manual refresh endpoint. Overlapping refreshes are skipped. The browser polls for updates and stores its language and theme preferences locally.

Telegram alerts are optional and sent on state changes. Failed sends are logged without retries. Next milestones: test Telegram delivery with real credentials and capture real dashboard screenshots. This early version has no authentication and is intentionally bound to loopback; it is not suitable for exposing on a network.
