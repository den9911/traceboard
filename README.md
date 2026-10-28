# Traceboard

![Traceboard illustrated cover](web/cover.svg)

**English** · [Русский](#русский)

A compact local dashboard for checking websites and APIs. Rust performs the checks; a lightweight browser interface shows the latest results.

> Early version: Telegram delivery has not yet been verified with a real bot. The cover is an illustration, not an application screenshot.

## Start

1. Install [Rust and Cargo](https://rustup.rs/).
2. Copy `targets.example.json` to `targets.json` and edit the service names and URLs. Set an interval between 15 and 3600 seconds.
3. Run `cargo run --release` and open <http://127.0.0.1:3000>. On Windows you can use `run.bat`; on macOS run `sh run.command` in Terminal. The launchers create `targets.json` from the example if needed.

The server listens on localhost only. Stop it with Ctrl+C. Restart after editing targets. Each check uses HTTP GET, follows up to three redirects and succeeds on a 2xx response. History is persisted in `traceboard.sqlite` (or the path in `TRACEBOARD_DB`) and retains the latest 120 checks per URL. The percentage is based on these samples, not a guaranteed uptime figure.

Use only target URLs you trust: the application sends network requests from your computer. Configuration and target URLs can be seen by anyone with access to the local dashboard. Do not put credentials or secrets in URLs.

## Telegram alerts

Create a bot with [@BotFather](https://t.me/BotFather), start it in the destination chat, and find the chat ID with Telegram's `getUpdates` API. Set both `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID` in the terminal before starting. Windows Command Prompt: `set TELEGRAM_BOT_TOKEN=...` and `set TELEGRAM_CHAT_ID=...`; macOS: `export TELEGRAM_BOT_TOKEN=...` and `export TELEGRAM_CHAT_ID=...`. Never commit these values. Alerts are sent when a service changes between healthy and unhealthy; the first check establishes a baseline. Failed notifications are logged and are not retried.

## Русский

Локальная панель для проверки сайтов и API. Проверки написаны на Rust; последние результаты отображаются в браузере.

> Ранняя версия: отправка в Telegram с настоящим ботом пока не проверена. Обложка — иллюстрация, не скриншот приложения.

Установите [Rust и Cargo](https://rustup.rs/), скопируйте `targets.example.json` в `targets.json` и укажите адреса. Запустите `cargo run --release` и откройте <http://127.0.0.1:3000>. Для Windows есть `run.bat`, для macOS — `sh run.command`; они создадут файл настроек из примера, если его нет.

Интервал задаётся от 15 до 3600 секунд. После изменения настроек перезапустите приложение. Успешным считается ответ HTTP 2xx; история хранится в `traceboard.sqlite` (путь можно задать через `TRACEBOARD_DB`), до 120 проверок на адрес. Процент рассчитан только по этим проверкам. Для уведомлений задайте в терминале `TELEGRAM_BOT_TOKEN` и `TELEGRAM_CHAT_ID` до запуска. Создайте бота через @BotFather и сначала напишите ему в нужном чате. Уведомления отправляются при изменении состояния; первая проверка задаёт исходное состояние. Ошибки отправки записываются в журнал, повторная отправка пока не предусмотрена. Сервер доступен только на локальном компьютере.

Добавляйте только доверенные адреса: приложение отправляет запросы из вашей сети. Не помещайте пароли и токены в URL.
