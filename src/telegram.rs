use crate::outbox::{Delivery, Outcome, Store};
use anyhow::Result;
use reqwest::{redirect::Policy, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{env, net::IpAddr, sync::Arc, time::Duration};

pub struct Telegram {
    endpoint: String,
    chat: String,
    pub destination: String,
}
#[derive(Deserialize)]
struct Reply {
    ok: bool,
    error_code: Option<u16>,
    parameters: Option<Parameters>,
    result: Option<SentMessage>,
}
#[derive(Deserialize)]
struct Parameters {
    retry_after: Option<u64>,
}
#[derive(Deserialize)]
struct SentMessage {
    message_id: i64,
}

impl Telegram {
    pub fn from_env() -> Result<Option<Self>> {
        let token = env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default();
        let chat = env::var("TELEGRAM_CHAT_ID").unwrap_or_default();
        if token.is_empty() && chat.is_empty() {
            return Ok(None);
        }
        anyhow::ensure!(
            !token.is_empty() && !chat.is_empty(),
            "Set both Telegram variables or neither / Задайте обе переменные Telegram или ни одной"
        );
        let (bot, secret) = token.split_once(':').ok_or_else(|| {
            anyhow::anyhow!("Invalid Telegram token format / Неверный формат токена Telegram")
        })?;
        anyhow::ensure!(
            !bot.is_empty()
                && bot.bytes().all(|c| c.is_ascii_digit())
                && !secret.is_empty()
                && token.len() <= 256
                && secret
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
            "Invalid Telegram token format / Неверный формат токена Telegram"
        );
        anyhow::ensure!(
            chat.len() <= 128 && !chat.chars().any(char::is_control),
            "Invalid Telegram chat / Неверный чат Telegram"
        );
        let base = env::var("TRACEBOARD_TELEGRAM_API")
            .unwrap_or_else(|_| "https://api.telegram.org".into());
        if base != "https://api.telegram.org" {
            let url = Url::parse(&base)
                .map_err(|_| anyhow::anyhow!("Invalid local Telegram fixture URL"))?;
            let local = url
                .host_str()
                .and_then(|h| h.trim_matches(['[', ']']).parse::<IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback());
            anyhow::ensure!(
                url.scheme() == "http"
                    && local
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && url.path() == "/",
                "Telegram override must be a loopback HTTP origin"
            );
        }
        // Bind queued events to the same bot ID/chat, allowing secret rotation for that bot.
        let destination = format!(
            "{:x}",
            Sha256::new()
                .chain_update(bot)
                .chain_update([0])
                .chain_update(&chat)
                .finalize()
        );
        Ok(Some(Self {
            endpoint: format!("{}/bot{token}/sendMessage", base.trim_end_matches('/')),
            chat,
            destination,
        }))
    }
    async fn send(&self, client: &reqwest::Client, delivery: &Delivery) -> Outcome {
        let response = client.post(&self.endpoint).json(&serde_json::json!({"chat_id":self.chat,"text":format!("{}\nEvent #{}",delivery.text,delivery.id)})).send().await;
        let Ok(mut response) = response else {
            return Outcome::Unknown;
        };
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if bytes.len() + chunk.len() <= 32768 => {
                    bytes.extend_from_slice(&chunk)
                }
                Ok(None) => break,
                _ => return Outcome::Unknown,
            }
        }
        classify_reply(status, &bytes)
    }
}
fn classify_reply(status: u16, bytes: &[u8]) -> Outcome {
    let Ok(reply) = serde_json::from_slice::<Reply>(bytes) else {
        return Outcome::Unknown;
    };
    if (200..300).contains(&status) && reply.ok {
        return match reply.result {
            Some(message) if message.message_id > 0 => Outcome::Delivered(message.message_id),
            _ => Outcome::Unknown,
        };
    }
    if !reply.ok && (status == 429 || (200..300).contains(&status)) && reply.error_code == Some(429)
    {
        return match reply.parameters.and_then(|p| p.retry_after) {
            Some(wait) if (1..=3600).contains(&wait) => Outcome::RateLimited(wait),
            _ => Outcome::Unknown,
        };
    }
    if !reply.ok
        && ((200..300).contains(&status) || (400..500).contains(&status))
        && reply
            .error_code
            .is_some_and(|code| (400..500).contains(&code) && code != 429)
    {
        return Outcome::Rejected;
    }
    Outcome::Unknown
}
pub async fn run(store: Arc<Store>, telegram: Telegram) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            store.stop_sender();
            return;
        }
    };
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let delivery = match store.claim(chrono::Utc::now().timestamp()).await {
            Ok(Some(value)) => value,
            Ok(None) => continue,
            Err(_) => {
                store.stop_sender();
                eprintln!("Notification storage unavailable; restart Traceboard after checking its database");
                return;
            }
        };
        let outcome = telegram.send(&client, &delivery).await;
        if store
            .finish(delivery, outcome, chrono::Utc::now().timestamp())
            .await
            .is_err()
        {
            store.stop_sender();
            eprintln!(
                "Notification confirmation could not be saved; delivery stopped, no blind retry"
            );
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn http_success_is_not_delivery_without_a_successful_api_result() {
        assert_eq!(
            classify_reply(200, br#"{"ok":true,"result":{"message_id":42}}"#),
            Outcome::Delivered(42)
        );
        assert_eq!(
            classify_reply(
                200,
                br#"{"ok":false,"error_code":403,"description":"private text"}"#
            ),
            Outcome::Rejected
        );
        assert_eq!(classify_reply(200, br#"{"ok":true}"#), Outcome::Unknown);
        assert_eq!(
            classify_reply(500, br#"{"ok":true,"result":{"message_id":42}}"#),
            Outcome::Unknown
        );
        assert_eq!(classify_reply(200, b"{"), Outcome::Unknown);
    }
    #[test]
    fn only_confirmed_bounded_rate_limits_allow_retry() {
        assert_eq!(
            classify_reply(
                429,
                br#"{"ok":false,"error_code":429,"parameters":{"retry_after":3}}"#
            ),
            Outcome::RateLimited(3)
        );
        assert_eq!(
            classify_reply(
                200,
                br#"{"ok":false,"error_code":429,"parameters":{"retry_after":4000}}"#
            ),
            Outcome::Unknown
        );
        assert_eq!(classify_reply(429, b"gateway error"), Outcome::Unknown);
        assert_eq!(
            classify_reply(500, br#"{"ok":false,"error_code":500}"#),
            Outcome::Unknown
        );
    }
}
