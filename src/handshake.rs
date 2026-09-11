//! The v1 feature surface: answering the confluxmap handshake.
//!
//! A confluxmap client sends `HELLO_C2S` as soon as it is in the world. The
//! server replies with `HELLO_POLICY_S2C` granting the seed and declaring
//! corrections off. That is the whole plugin: the client renders the predicted
//! map from the seed on its own.
//!
//! Gate-keeping comes for free from the event model. Only a client that has
//! confluxmap installed registers `confluxmap:map_sync` and sends a HELLO, so a
//! vanilla client never produces this event at all - there is no player
//! filtering to do here.

use pumpkin_plugin_api::Player;
use tracing::{info, warn};

use crate::config::Config;
use crate::protocol::{self, HelloC2S, flags};
use crate::state;

/// What happened to one inbound `confluxmap:map_sync` payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not a HELLO frame; another message type on the same channel, or garbage.
    NotHello {
        /// The leading type byte, for the log.
        type_byte: Option<u8>,
    },
    /// A policy was built and accepted by the transport.
    Replied {
        /// One-line human-readable summary.
        summary: String,
        /// Size of the frame that was sent.
        bytes: usize,
    },
    /// A policy was built but the transport refused to send it.
    SendFailed {
        /// One-line human-readable summary.
        summary: String,
    },
}

/// Builds the `HELLO_POLICY_S2C` frame for `config`.
///
/// Split out from [`handle_payload`] so the wire bytes can be produced and
/// asserted without a live player.
pub fn build_policy(config: &Config, server_version: Option<&str>) -> Vec<u8> {
    let flags = if config.grants_seed() {
        flags::SEED_GRANTED
    } else {
        0
    };
    protocol::build_hello_policy(
        flags,
        &config.world_id(),
        &config.resolve_worldgen(server_version),
        config.budgets,
        &config.dims_for_policy(),
    )
}

/// Handles one inbound payload on [`protocol::CHANNEL_ID`].
///
/// `server_version` is the running server's `pumpkin-version`, used to derive
/// the worldgen version the client feeds to cubiomes.
pub fn handle_payload(player: &Player, data: &[u8], server_version: Option<&str>) -> Outcome {
    let Some(hello) = protocol::parse_hello_c2s(data) else {
        return Outcome::NotHello {
            type_byte: data.first().copied(),
        };
    };

    let index = state::count_hello();
    let config = state::config_snapshot();
    let policy = build_policy(&config, server_version);
    state::remember_policy(&policy);

    if !config.grants_seed() {
        warn!(
            "[confluxmap] no seed to grant ({}); replying with seedGranted=0 so the client is \
             not left waiting. Set {} in [plugins.overrides.confluxmap-pumpkin.environment].",
            if config.seed.is_none() {
                "CFM_SEED is unset or unparseable"
            } else {
                "share_seed=false"
            },
            crate::config::ENV_SEED
        );
    }

    let sent = send(player, &policy);
    let summary = summarise(
        player,
        &hello,
        &config,
        server_version,
        &policy,
        index,
        sent,
    );

    if sent {
        state::count_policy_sent();
        info!("[confluxmap] {summary}");
        state::remember_handshake(summary.clone());
        Outcome::Replied {
            summary,
            bytes: policy.len(),
        }
    } else {
        state::count_send_failure();
        warn!("[confluxmap] {summary}");
        state::remember_handshake(summary.clone());
        Outcome::SendFailed { summary }
    }
}

/// Sends a plugin message. Plugin messaging only exists on the Java side, so
/// this returns false for a Bedrock client.
pub fn send(player: &Player, data: &[u8]) -> bool {
    match player.as_java() {
        Some(java) => {
            java.send_custom_payload(protocol::CHANNEL_ID, data);
            true
        }
        None => false,
    }
}

fn summarise(
    player: &Player,
    hello: &HelloC2S,
    config: &Config,
    server_version: Option<&str>,
    policy: &[u8],
    index: u64,
    sent: bool,
) -> String {
    let client = match player.as_java() {
        Some(java) => format!(
            "client={:?} brand={:?}",
            java.get_version(),
            java.get_brand()
        ),
        None => "client=bedrock".to_string(),
    };
    format!(
        "HELLO #{index} {} modVersion={:?} predictor={:?} {client} -> HELLO_POLICY {} {}B \
         seedGranted={} seed={} worldgen={:?} worldId={} dims={}",
        player.get_name(),
        hello.mod_version,
        hello.predictor_version,
        if sent { "sent" } else { "NOT sent" },
        policy.len(),
        config.grants_seed(),
        if config.grants_seed() {
            config.advertised_seed().to_string()
        } else {
            "-".to_string()
        },
        config.resolve_worldgen(server_version),
        config.world_id(),
        config.dims.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_config() -> Config {
        Config {
            seed: Some(0x0123_4567_89AB_CDEF),
            ..Config::default()
        }
    }

    /// The full pipeline, against the vector the reference encoder produced.
    #[test]
    fn build_policy_matches_the_reference_vector() {
        let policy = build_policy(&seeded_config(), Some("0.1.0-dev+26.2-26.45"));
        let golden = "0201002430303030303030302d303030302d303030302d303030302d343536373839616263646566000432362e320004000000080064040100136d696e6563726166743a6f766572776f726c6400096f766572776f726c64030123456789abcdef";
        assert_eq!(state::hex(&policy), golden);
    }

    /// A server whose version string cannot be parsed still resolves the
    /// worldgen version rather than emitting an empty one.
    #[test]
    fn unparseable_server_version_falls_back_cleanly() {
        let policy = build_policy(&seeded_config(), Some("garbage"));
        // worldgen sits after the worldId; check the fallback appears on the wire.
        let hex = state::hex(&policy);
        let expected_worldgen = state::hex(crate::config::FALLBACK_MC_VERSION.as_bytes());
        assert!(
            hex.contains(&expected_worldgen),
            "fallback worldgen {expected_worldgen} missing from {hex}"
        );
    }

    #[test]
    fn disabled_sharing_clears_the_seed_flag_and_zeroes_the_seed_on_the_wire() {
        let config = Config {
            seed: Some(7),
            share_seed: false,
            ..Config::default()
        };
        let withheld = build_policy(&config, None);
        assert_eq!(withheld[0], protocol::MSG_HELLO_POLICY_S2C);
        assert_eq!(
            withheld[1], 0,
            "no flags may be set when the seed is withheld"
        );
        // Same shape as a granting policy, so the client's parser sees the same
        // field layout; only the flag and the seed differ.
        assert_eq!(withheld.len(), build_policy(&seeded_config(), None).len());
        assert_eq!(
            &withheld[withheld.len() - 8..],
            &[0u8; 8],
            "the per-dim seed must be zero when the seed is withheld"
        );
    }
}
