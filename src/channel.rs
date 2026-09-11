//! Declaring `confluxmap:map_sync` to the client.
//!
//! Registering event handlers is only half of a plugin-message channel. A
//! confluxmap client refuses to send its HELLO unless the **server has already
//! declared the channel to it**: `ClientPlayNetworking.canSend` gates on
//! `ClientPacketListener.hasChannel(id)`, and when that is false the client
//! silently skips the handshake (the failure is logged at debug level only).
//!
//! A Bukkit server announces channels for its plugins automatically. Pumpkin has
//! no equivalent API, so the plugin emits the vanilla packet itself: a
//! `minecraft:register` custom payload whose body is the newline-free,
//! NUL-separated channel list. That is exactly the packet a Paper server sends
//! on the plugin's behalf.
//!
//! This is why the channel must be declared **on login/join**, not lazily in
//! reply to something: the client sends HELLO from its own join callback, so a
//! declaration that arrives afterwards is too late for that session.

use pumpkin_plugin_api::Player;

use crate::protocol;

/// The vanilla control channel used to announce plugin-message channels.
pub const REGISTER_CHANNEL: &str = "minecraft:register";

/// Body of a `minecraft:register` payload for `channels`.
///
/// Names are NUL-separated with a trailing separator, matching what
/// `ClientPacketListener.handleCustomPayload` splits on.
pub fn register_body(channels: &[&str]) -> Vec<u8> {
    let mut body = Vec::new();
    for channel in channels {
        body.extend_from_slice(channel.as_bytes());
        body.push(0);
    }
    body
}

/// Announces the companion channel to `player`.
///
/// Idempotent from the client's point of view: the client keeps a set, so a
/// repeated announcement is a no-op. Returns `false` when the player is a
/// Bedrock client with no Java connection to send on.
pub fn declare_companion_channel(player: &Player) -> bool {
    let Some(java) = player.as_java() else {
        return false;
    };
    let body = register_body(&[protocol::CHANNEL_ID]);
    java.send_custom_payload(REGISTER_CHANNEL, &body);
    true
}

/// Which side of the join sequence a declaration happened on, for logging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Fired before the client's own join callback - the only useful stage.
    Login,
    /// Fired around the join; kept as a second attempt.
    Join,
    /// Fired in reply to a late channel registration from the client.
    Late,
}

impl Stage {
    /// A short tag for log lines.
    pub fn tag(self) -> &'static str {
        match self {
            Stage::Login => "login",
            Stage::Join => "join",
            Stage::Late => "late",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_body_is_nul_separated_with_a_trailing_separator() {
        assert_eq!(
            register_body(&["confluxmap:map_sync"]),
            b"confluxmap:map_sync\0"
        );
    }

    #[test]
    fn register_body_handles_several_channels() {
        assert_eq!(register_body(&["a", "b"]), b"a\0b\0");
    }

    #[test]
    fn the_companion_channel_name_is_the_protocol_one() {
        assert_eq!(protocol::CHANNEL_ID, "confluxmap:map_sync");
        let body = register_body(&[protocol::CHANNEL_ID]);
        assert!(body.ends_with(b"\0"));
        assert_eq!(body.len(), protocol::CHANNEL_ID.len() + 1);
    }
}
