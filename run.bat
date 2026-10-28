@echo off
cd /d "%~dp0"
where cargo >nul 2>&1 || (echo Install Rust from https://rustup.rs/ & exit /b 1)
if not exist targets.json copy targets.example.json targets.json >nul
start "" http://127.0.0.1:3000
cargo run --release
pause
