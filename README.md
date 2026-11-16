# simple-alert

A command-line tool and Rust library that posts an alert from a service to one Telegram chat through a bot.

## Install

```sh
cargo install --git https://github.com/zacharysiegel/simple-alert
```

## Setup

1. In Telegram, send `/newbot` to [@BotFather](https://t.me/BotFather) and keep the token it returns.
2. Create a channel (a group also works).
3. Add the bot to the channel as an administrator allowed to post messages.
4. Run `simple-alert register`. It asks for the token (hidden) and checks it with Telegram, asks you to post any message in the channel and press Enter, lists the channels and groups the bot has seen for you to pick by number (or asks for the chat id if it saw none), asks before replacing a different token already in the store, writes the config and the token, and offers to send a test alert.

`register` creates `~/.config/simple-alert/` and its files readable only by you. When the config has no `[secrets]` section it generates a secr key, and refuses to run if the store already holds secrets, since those were encrypted with another key. Rerunning it rewrites the config with the newly chosen chat. Telegram's `getUpdates`, which lists the chats, does not work while the bot has a webhook set.

## Commands

- `simple-alert send --service <name> --level <info|warning|error> <message>`: posts `<LEVEL> [<service>] <message>`, for example `ERROR [xost] tweet id 2 was marked failed`.
    - Service name: letters, digits, `-` and `_`.
    - Message: trimmed; an empty message is rejected. Text over Telegram's 4096-character limit is cut and ends with `[truncated]`.
    - Retries: up to 3 attempts on a network error, HTTP 5xx, or HTTP 429. A 429 waits the `retry_after` Telegram gives, and fails at once if that exceeds 30 seconds. Other 4xx responses fail at once.
- `simple-alert test`: posts `INFO [simple-alert] test alert` the same way.
- `simple-alert register`: interactive setup (see Setup).
- `--config <file>` (any command): config path, default `~/.config/simple-alert/config.toml`.

## Config

```toml
[telegram]
chat_id = -1001234567890

[secrets]
store = "~/.config/simple-alert/secrets.yaml"
key = "<base64 secr key>"
```

The bot token is the `telegram_bot_token` entry of the [secr](https://crates.io/crates/secr) store. `~/` is expanded in `store`. Missing keys, unknown keys, a non-integer or zero `chat_id`, and a `key` that does not decode to 32 bytes are rejected.

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
