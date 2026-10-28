#!/bin/sh
cd "$(dirname "$0")" || exit 1
command -v cargo >/dev/null 2>&1 || { echo 'Install Rust from https://rustup.rs/'; exit 1; }
[ -f targets.json ] || cp targets.example.json targets.json
(sleep 4; open http://127.0.0.1:3000) &
cargo run --release
