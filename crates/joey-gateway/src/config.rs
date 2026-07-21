//! Gateway configuration types (port of the relevant parts of upstream
//! `gateway/config.py`).
//!
//! Currently this module carries the [`Platform`] enum. Upstream `Platform`
//! is a Python `Enum` with dynamic `_missing_` members: unknown platform
//! names are accepted (lowercased) when they correspond to a bundled or
//! runtime-registered plugin platform, and rejected otherwise. The Rust port
//! models dynamic members as [`Platform::Other`]; because there is no plugin
//! registry to consult yet, any non-empty name parses to `Other` rather than
//! being rejected (this is the one intentional relaxation vs upstream).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Supported messaging platforms (port of `gateway/config.py::Platform`).
///
/// The `Display`/serde representation is the lowercase wire value used by
/// upstream (`platform.value`), e.g. `"whatsapp_cloud"`, `"api_server"`.
/// Parsing is case-insensitive and trims surrounding whitespace, matching
/// upstream `_missing_` normalization.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Platform {
    Local,
    Telegram,
    Discord,
    Whatsapp,
    WhatsappCloud,
    Slack,
    Signal,
    Mattermost,
    Matrix,
    Homeassistant,
    Email,
    Sms,
    Dingtalk,
    ApiServer,
    Webhook,
    MsgraphWebhook,
    Feishu,
    Wecom,
    WecomCallback,
    Weixin,
    Bluebubbles,
    Qqbot,
    Yuanbao,
    /// Generic relay adapter fronted by the connector (EXPERIMENTAL).
    Relay,
    /// Dynamic member for plugin platforms (upstream `_missing_` pseudo-members,
    /// e.g. `Platform("irc")`). Always stores the normalized (trimmed,
    /// lowercased) wire value.
    Other(String),
}

impl Platform {
    /// The lowercase wire value (upstream `platform.value`).
    pub fn as_str(&self) -> &str {
        match self {
            Platform::Local => "local",
            Platform::Telegram => "telegram",
            Platform::Discord => "discord",
            Platform::Whatsapp => "whatsapp",
            Platform::WhatsappCloud => "whatsapp_cloud",
            Platform::Slack => "slack",
            Platform::Signal => "signal",
            Platform::Mattermost => "mattermost",
            Platform::Matrix => "matrix",
            Platform::Homeassistant => "homeassistant",
            Platform::Email => "email",
            Platform::Sms => "sms",
            Platform::Dingtalk => "dingtalk",
            Platform::ApiServer => "api_server",
            Platform::Webhook => "webhook",
            Platform::MsgraphWebhook => "msgraph_webhook",
            Platform::Feishu => "feishu",
            Platform::Wecom => "wecom",
            Platform::WecomCallback => "wecom_callback",
            Platform::Weixin => "weixin",
            Platform::Bluebubbles => "bluebubbles",
            Platform::Qqbot => "qqbot",
            Platform::Yuanbao => "yuanbao",
            Platform::Relay => "relay",
            Platform::Other(value) => value,
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an empty/whitespace-only platform name
/// (upstream: `Platform("")` raises `ValueError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidPlatformError;

impl fmt::Display for InvalidPlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid platform name: empty or whitespace-only")
    }
}

impl std::error::Error for InvalidPlatformError {}

impl FromStr for Platform {
    type Err = InvalidPlatformError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_lowercase();
        if normalized.is_empty() {
            return Err(InvalidPlatformError);
        }
        Ok(match normalized.as_str() {
            "local" => Platform::Local,
            "telegram" => Platform::Telegram,
            "discord" => Platform::Discord,
            "whatsapp" => Platform::Whatsapp,
            "whatsapp_cloud" => Platform::WhatsappCloud,
            "slack" => Platform::Slack,
            "signal" => Platform::Signal,
            "mattermost" => Platform::Mattermost,
            "matrix" => Platform::Matrix,
            "homeassistant" => Platform::Homeassistant,
            "email" => Platform::Email,
            "sms" => Platform::Sms,
            "dingtalk" => Platform::Dingtalk,
            "api_server" => Platform::ApiServer,
            "webhook" => Platform::Webhook,
            "msgraph_webhook" => Platform::MsgraphWebhook,
            "feishu" => Platform::Feishu,
            "wecom" => Platform::Wecom,
            "wecom_callback" => Platform::WecomCallback,
            "weixin" => Platform::Weixin,
            "bluebubbles" => Platform::Bluebubbles,
            "qqbot" => Platform::Qqbot,
            "yuanbao" => Platform::Yuanbao,
            "relay" => Platform::Relay,
            _ => Platform::Other(normalized),
        })
    }
}

impl Serialize for Platform {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Platform {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_wire_values_round_trip() {
        for value in [
            "local",
            "telegram",
            "discord",
            "whatsapp",
            "whatsapp_cloud",
            "slack",
            "signal",
            "mattermost",
            "matrix",
            "homeassistant",
            "email",
            "sms",
            "dingtalk",
            "api_server",
            "webhook",
            "msgraph_webhook",
            "feishu",
            "wecom",
            "wecom_callback",
            "weixin",
            "bluebubbles",
            "qqbot",
            "yuanbao",
            "relay",
        ] {
            let platform: Platform = value.parse().unwrap();
            assert!(!matches!(platform, Platform::Other(_)), "{value} parsed as Other");
            assert_eq!(platform.as_str(), value);
            assert_eq!(platform.to_string(), value);
        }
    }

    #[test]
    fn from_str_is_case_insensitive_and_trims() {
        assert_eq!("TELEGRAM".parse::<Platform>().unwrap(), Platform::Telegram);
        assert_eq!(" Telegram ".parse::<Platform>().unwrap(), Platform::Telegram);
        assert_eq!("WhatsApp".parse::<Platform>().unwrap(), Platform::Whatsapp);
    }

    #[test]
    fn unknown_names_become_normalized_other() {
        assert_eq!("IRC".parse::<Platform>().unwrap(), Platform::Other("irc".into()));
        assert_eq!("irc".parse::<Platform>().unwrap().to_string(), "irc");
    }

    #[test]
    fn empty_names_are_rejected() {
        assert!("".parse::<Platform>().is_err());
        assert!("   ".parse::<Platform>().is_err());
    }

    #[test]
    fn serde_uses_wire_values() {
        let json = serde_json::to_string(&Platform::WhatsappCloud).unwrap();
        assert_eq!(json, "\"whatsapp_cloud\"");
        let parsed: Platform = serde_json::from_str("\"telegram\"").unwrap();
        assert_eq!(parsed, Platform::Telegram);
        let other: Platform = serde_json::from_str("\"irc\"").unwrap();
        assert_eq!(other, Platform::Other("irc".into()));
        assert!(serde_json::from_str::<Platform>("\"\"").is_err());
    }
}
