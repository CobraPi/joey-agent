# joey-gateway — Messaging Gateway Spine

Platform-neutral core (port of upstream `gateway/`). **No concrete platform
adapters ship yet** — Telegram/Discord/Slack etc. adapters are added behind
the trait incrementally. The spine is what cron delivery routing and future
platform integrations build on (session keys, message events, adapter
contract).

## Platform enum

Wire values (case-insensitive parse): `local`, `telegram`, `discord`,
`whatsapp`, `whatsapp_cloud`, `slack`, `signal`, `mattermost`, `matrix`,
`homeassistant`, `email`, `sms`, `dingtalk`, `api_server`, `webhook`,
`msgraph_webhook`, `feishu`, `wecom`, `wecom_callback`, `weixin`,
`bluebubbles`, `qqbot`, `yuanbao`, `relay` (experimental), plus `Other(...)`
for plugin platforms (any non-empty name parses — one intentional relaxation
vs upstream).

## SessionSource and session keys

`SessionSource` describes message origin: platform, chat_id / chat_name /
chat_type (dm|group|channel|thread), user ids, thread/forum ids, and scope
discriminators (Discord guild, Slack workspace). It serializes
byte-identically to upstream `to_dict`/`from_dict`. `build_session_key`
derives the stable session identifier used across the CLI, the sessions DB,
and cron delivery routing.

## MessageEvent / SendResult / PlatformAdapter trait

Adapters implement `connect(is_reconnect)`, `disconnect`, `send`,
`get_chat_info`, and declare capabilities:

- `supports_code_blocks`
- `supports_status_text` (text typing indicator)
- `supports_async_delivery`
- `splits_long_messages`
- `typed_command_prefix` (`/` or `!`)

The base layer also provides UTF-16-aware `truncate_message` (default 4096
chars) and send-error classification (retryable vs fatal patterns).

## WhatsApp identity

JID/LID canonicalization and alias expansion (`whatsapp_identity.rs`).
