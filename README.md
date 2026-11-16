# simple-alert

A command-line tool and Rust library that posts an alert from a service to that service's Telegram chat through one bot.

## Install

```sh
cargo install --git https://github.com/zacharysiegel/simple-alert
```

## Setup

Create the bot once: in Telegram, send `/newbot` to [@BotFather](https://t.me/BotFather) and keep the token it returns.

For each service:

1. Create a channel (a group also works).
2. Add the bot to the channel as an administrator allowed to post messages.
3. Run `simple-alert register <service>`.

`register <service>`:

1. If the service already has a chat, shows it and asks `Replace it? [y/N]`; declining changes nothing.
2. Uses the stored bot token if there is one; otherwise asks for it (hidden). Checks the token with Telegram and names the bot. If Telegram rejects the stored token, offers to enter a new one; a network error stops register.
3. Asks you to post any message in the service's channel and press Enter, then lists the channels and groups the bot has seen for you to pick by number or by typing a chat id (or asks for the chat id if it saw none). Chats another service already uses are labeled; a chat may serve several services.
4. Asks before replacing a different token already in the store, writes the service's chat to the config and the token to the store if it is new, and offers to send a test alert. The config is read again just before writing, so changes made meanwhile are kept; if they changed this service's chat, it asks again before replacing it.

`register` creates `~/.config/simple-alert/` and its files readable only by you. When the config has no `[secrets]` section it generates a secr key, and refuses to run if the store already holds secrets, since those were encrypted with another key. Telegram's `getUpdates`, which lists the chats, does not work while the bot has a webhook set.

## Commands

- `simple-alert send --service <name> --level <info|warning|error> <message>`: posts `<LEVEL> [<service>] <message>` to the service's chat, for example `ERROR [xost] tweet id 2 was marked failed`.
    - Service name: letters, digits, `-` and `_`. A service without a `[services.<name>]` table is rejected.
    - Message: trimmed; an empty message is rejected. Text over Telegram's 4096-character limit is cut and ends with `[truncated]`.
    - Retries: up to 3 attempts on a network error, HTTP 5xx, or HTTP 429. A 429 waits the `retry_after` Telegram gives, and fails at once if that exceeds 30 seconds. Other 4xx responses fail at once.
- `simple-alert test <service>`: posts `INFO [<service>] test alert` the same way.
- `simple-alert register <service>`: interactive setup (see Setup).
- `--config <file>` (any command): config path, default `~/.config/simple-alert/config.toml`.

## Config

```toml
[secrets]
store = "~/.config/simple-alert/secrets.yaml"
key = "<base64 secr key>"

[services.xost]
chat_id = -1001234567890

[services.backup]
chat_id = -1009876543210
```

The bot token is the `telegram_bot_token` entry of the [secr](https://crates.io/crates/secr) store. `~/` is expanded in `store`. Missing keys, unknown keys, an invalid service name, a non-integer or zero `chat_id`, and a `key` that does not decode to 32 bytes are rejected. A config with the former `[telegram]` section is rejected; running `register` for each service replaces it.

## Exit codes

- `0`: the alert was delivered (for `register`: setup finished).
- `1`: any other failure, with the reason on standard error. The token is never printed.
- `2`: invalid command-line arguments.

## Calling it from a service

From a script, ignoring alert failures so they do not fail the job:

```sh
simple-alert send --service backup --level error "nightly backup failed: ${reason}" || true
```

From Rust, with `simple-alert = { git = "https://github.com/zacharysiegel/simple-alert" }`:

```rust
use simple_alert::alert::Alert;
use simple_alert::alert::AlertLevel;

let alert: Alert = Alert::new("xost", AlertLevel::Error, "tweet id 2 was marked failed")?;
simple_alert::send::send_alert(&simple_alert::config::default_config_path()?, &alert)?;
```

To send several alerts with one loaded config and HTTP agent, call `send::create_alert_client(&Config::load(path)?)?` once and `.send(&alert)` on the result. Errors are `simple_alert::error::AppError` ([minimer](https://crates.io/crates/minimer)); a minimer crate absorbs them with `minimer::impl_from_error!(AppError, simple_alert::error::AppError);`.

The bot token is part of each request URL, and `ureq` logs request URLs at trace level through the `log` crate. A service that enables logging must keep the `ureq` target below trace.
