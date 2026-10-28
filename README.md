# Traceboard

![Traceboard illustrated cover](web/cover.svg)

**English** · [Русский](#русский)

A compact local dashboard for checking websites and APIs. Rust performs the checks; a lightweight browser interface shows the latest results.

> Early version: the Rust build and live checks have not yet been verified. The cover is an illustration, not an application screenshot.

## Start

1. Install [Rust and Cargo](https://rustup.rs/).
2. Copy `targets.example.json` to `targets.json` and edit the service names and URLs. Set an interval between 15 and 3600 seconds.
3. Run `cargo run --release` and open <http://127.0.0.1:3000>. On Windows you can use `run.bat`; on macOS run `sh run.command` in Terminal. The launchers create `targets.json` from the example if needed.

The server listens on localhost only. Stop it with Ctrl+C. Restart after editing targets. Each check uses HTTP GET, follows up to three redirects and succeeds on a 2xx response. History holds at most 120 checks per service in memory and is cleared on restart; the percentage is based on these samples, not a guaranteed uptime figure. Telegram notifications and persistent history are not yet implemented.

Use only target URLs you trust: the application sends network requests from your computer. Configuration and target URLs can be seen by anyone with access to the local dashboard. Do not put credentials or secrets in URLs.

## Русский

Локальная панель для проверки сайтов и API. Проверки написаны на Rust; последние результаты отображаются в браузере.

> Ранняя версия: сборка Rust и реальные сетевые проверки пока не проверены. Обложка — иллюстрация, не скриншот приложения.

Установите [Rust и Cargo](https://rustup.rs/), скопируйте `targets.example.json` в `targets.json` и укажите адреса. Запустите `cargo run --release` и откройте <http://127.0.0.1:3000>. Для Windows есть `run.bat`, для macOS — `sh run.command`; они создадут файл настроек из примера, если его нет.

Интервал задаётся от 15 до 3600 секунд. После изменения настроек перезапустите приложение. Успешным считается ответ HTTP 2xx; история хранит до 120 проверок на сервис в памяти и очищается после перезапуска. Процент рассчитан только по этим проверкам. Уведомления в Telegram и постоянное хранение истории пока не реализованы. Сервер доступен только на локальном компьютере.

Добавляйте только доверенные адреса: приложение отправляет запросы из вашей сети. Не помещайте пароли и токены в URL.
