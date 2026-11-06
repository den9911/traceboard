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

Create a bot with [@BotFather](https://t.me/BotFather), start it in the destination chat, and find the chat ID with Telegram's `getUpdates` API. Set both `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID` in the terminal before starting. Windows Command Prompt: `set TELEGRAM_BOT_TOKEN=...` and `set TELEGRAM_CHAT_ID=...`; macOS: `export TELEGRAM_BOT_TOKEN=...` and `export TELEGRAM_CHAT_ID=...`. Never commit these values. Alerts are sent when a service changes between healthy and unhealthy; the first check establishes a baseline. State changes are committed to a bounded SQLite outbox together with their checks. Delivery continues independently; the dashboard shows queued, delivered, rejected, unconfirmed and overflow counts. Only a confirmed rate-limit rejection allows up to three attempts. Unknown outcomes are not retried blindly. The `failure_threshold` setting controls how many consecutive failures trigger a down state (default 3).

## Русский

Локальная панель для проверки сайтов и API. Проверки написаны на Rust; последние результаты отображаются в браузере.

> Ранняя версия: отправка в Telegram с настоящим ботом пока не проверена. Обложка — иллюстрация, не скриншот приложения.

Установите [Rust и Cargo](https://rustup.rs/), скопируйте `targets.example.json` в `targets.json` и укажите адреса. Запустите `cargo run --release` и откройте <http://127.0.0.1:3000>. Для Windows есть `run.bat`, для macOS — `sh run.command`; они создадут файл настроек из примера, если его нет.

Интервал задаётся от 15 до 3600 секунд. После изменения настроек перезапустите приложение. Успешным считается ответ HTTP 2xx; история хранится в `traceboard.sqlite` (путь можно задать через `TRACEBOARD_DB`), до 120 проверок на адрес. Процент рассчитан только по этим проверкам. Для уведомлений задайте в терминале `TELEGRAM_BOT_TOKEN` и `TELEGRAM_CHAT_ID` до запуска. Создайте бота через @BotFather и сначала напишите ему в нужном чате. Уведомления отправляются при изменении состояния; первая проверка задаёт исходное состояние. Событие уведомления сохраняется в ограниченной SQLite-очереди вместе с проверкой. Панель показывает ожидание, доставку, отказ, неизвестный исход и переполнение. До трёх попыток разрешены только после явного отказа из-за лимита запросов; неизвестная доставка не повторяется вслепую. Настройка `failure_threshold` задаёт число неудачных проверок подряд для перехода в состояние ошибки (по умолчанию 3). Сервер доступен только на локальном компьютере.

Добавляйте только доверенные адреса: приложение отправляет запросы из вашей сети. Не помещайте пароли и токены в URL.

### HTTP rules / Правила HTTP
Optional target fields `expected_statuses` (1–50 HTTP codes) and `latency_warning_ms` (1–60000) control success and a separate warning. Legacy targets still accept 2xx. Latency measures time until response headers, not a full body download. A slow successful response remains healthy. Rules and diagnostics use no saved response bodies. Telegram uses a separate sender and a 256-record persistent queue. One message is attempted at a time, with at least one second between starts; slow delivery does not hold the check loop. A full queue increments a persistent overflow count while checks are retained.
Необязательные поля цели: `expected_statuses` и `latency_warning_ms`. Предупреждение о задержке не меняет успешный статус. Задержка — до заголовков. При занятом отправителе Telegram события ждут в очереди; при её переполнении проверки сохраняются, а пропуск учитывается явно.
Startup diagnostics: `cargo run --release -- --check` validates configuration, writable data location, local port and Telegram variable pair without network requests. / Проверка запуска без сетевых запросов: `--check`.

### Delivery contract / Контракт доставки
Pending events survive restart and stay bound to the same bot ID and chat. Changing the destination pauses the old queue. An interrupted in-flight attempt becomes unconfirmed rather than being sent again; inspect Telegram first. HTTP success alone is not a delivery receipt. The dashboard counts retained queue records, not lifetime deliveries; overflow is cumulative. Known delivered/rejected records are pruned, while unconfirmed records require attention and continue to occupy capacity. Do not delete the database to clear a warning: preserve it and investigate. There is no automatic resend button for uncertain delivery.

Очередь переживает перезапуск и привязана к ID бота и чату. При изменении получателя прежние события приостанавливаются. Прерванная отправка считается неподтверждённой и не повторяется автоматически: сначала проверьте Telegram. Успешный HTTP сам по себе не подтверждает доставку. Счётчики показывают сохранённые записи очереди, не все доставки за всё время; переполнение учитывается накопительно. Старые подтверждённые доставки и отказы удаляются из журнала, неизвестные исходы занимают место до разбирательства. Не удаляйте базу ради сброса предупреждения.
