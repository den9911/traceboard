@echo off
cd /d "%~dp0"
where cargo >nul 2>&1 || (echo Install Rust from https://rustup.rs/ & exit /b 1)
if not exist targets.json copy targets.example.json targets.json >nul
cargo run --release -- %*
exit /b %errorlevel%
