//! Play: the connection to a running machine (its WireGuard tunnel, when
//! it has WireGuard on), pairing Moonlight and starting the stream. One
//! machine at a time, like the rest of ReGa.

use std::sync::{LazyLock, Mutex};

use rega_core::stream::moonlight::Moonlight;
use rega_core::stream::{self, PlayOutcome, Streams};

use crate::api::rega::{RegaError, RegaResult};
use crate::frb_generated::StreamSink;
use crate::registry;

/// The connection Play keeps.
static STREAMS: LazyLock<Streams> = LazyLock::new(Streams::new);
/// The provider of the machine it's connected to.
static PROVIDER: Mutex<Option<String>> = Mutex::new(None);

pub enum PlayStep {
    Connecting,
    Pairing,
    StartingMoonlight,
    Done,
}

/// Where Play has got to.
pub struct PlayUpdate {
    pub step: PlayStep,
    /// Once done, when ReGa can't run Moonlight itself (as in the Flatpak):
    /// the host to connect to from Moonlight's own window. `None` when
    /// Moonlight is streaming.
    pub moonlight_host: Option<String>,
    /// With `moonlight_host`: whether Sunshine already has this device
    /// paired. Only a hint, since Moonlight may have forgotten it.
    pub paired: bool,
}

/// What Play is connected to.
pub struct StreamStatus {
    pub provider_id: String,
    pub machine_id: String,
    pub host: String,
    /// Through the WireGuard tunnel, which ends when ReGa quits.
    pub wireguard: bool,
}

/// Connects to the machine, pairs Moonlight if it isn't, and starts the
/// stream, reporting each step. Ends with [`PlayStep::Done`]; an error ends
/// the stream instead, as its message.
pub async fn play(provider_id: String, machine_id: String, updates: StreamSink<PlayUpdate>) {
    // A stream function's own error never reaches the stream, so failures
    // go through it. An add that fails means the UI stopped listening.
    let result = play_with(&provider_id, &machine_id, &|update| {
        let _ = updates.add(update);
    })
    .await;
    if let Err(e) = result {
        let _ = updates.add_error(e.message);
    }
}

async fn play_with(
    provider_id: &str,
    machine_id: &str,
    send: &(dyn Fn(PlayUpdate) + Send + Sync),
) -> RegaResult<()> {
    let step = |step| PlayUpdate {
        step,
        moonlight_host: None,
        paired: false,
    };
    send(step(PlayStep::Connecting));
    let entry = registry::entry(provider_id)?;
    // A machine of another provider can't share the tunnel.
    let other_provider = PROVIDER
        .lock()
        .unwrap()
        .as_deref()
        .is_some_and(|p| p != provider_id);
    if other_provider {
        STREAMS.close().await;
    }
    let target = entry.provider().stream_target(machine_id).await?;
    let moonlight = Moonlight::find().await;
    *PROVIDER.lock().unwrap() = Some(provider_id.into());
    let progress = |s: stream::PlayStep| {
        send(step(match s {
            stream::PlayStep::Connecting => PlayStep::Connecting,
            stream::PlayStep::Pairing => PlayStep::Pairing,
            stream::PlayStep::StartingMoonlight => PlayStep::StartingMoonlight,
        }))
    };
    let outcome = STREAMS.play(target, moonlight.as_ref(), &progress).await?;
    send(match outcome {
        PlayOutcome::Streaming => step(PlayStep::Done),
        PlayOutcome::OpenMoonlight { host, paired } => PlayUpdate {
            step: PlayStep::Done,
            moonlight_host: Some(host),
            paired,
        },
    });
    Ok(())
}

/// Hands the PIN Moonlight shows to the pairing request it made to the
/// machine Play connected to.
pub async fn pair_with_pin(provider_id: String, machine_id: String, pin: String) -> RegaResult<()> {
    if PROVIDER.lock().unwrap().as_deref() != Some(provider_id.as_str()) {
        return Err(RegaError::from(stream::StreamError::NotConnected));
    }
    Ok(STREAMS.pair_with_pin(&machine_id, &pin).await?)
}

/// What Play is connected to, if anything.
#[flutter_rust_bridge::frb(sync)]
pub fn stream_status() -> Option<StreamStatus> {
    let provider_id = PROVIDER.lock().unwrap().clone()?;
    STREAMS.status().map(|s| StreamStatus {
        provider_id,
        machine_id: s.machine_id,
        host: s.host,
        wireguard: s.wireguard,
    })
}

/// Closes Play's connection, which ends a stream through the tunnel.
pub async fn close_tunnel() {
    STREAMS.close().await;
}

/// Before a change that takes the machine down or gives it a new IP (stop,
/// destroy, a running switch): its connection goes first.
pub(crate) async fn close_for(provider_id: &str, machine_id: &str) {
    if PROVIDER.lock().unwrap().as_deref() == Some(provider_id) {
        STREAMS.close_machine(machine_id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::rega::{launch_machine, stop_machine};

    #[tokio::test]
    async fn demo_machines_say_they_have_nothing_to_stream() {
        crate::api::rega::tests::demo();
        let m = launch_machine(
            "aws".into(),
            "Play test".into(),
            "eu-north-1".into(),
            "t3.medium".into(),
            false,
            true,
            false,
        )
        .await
        .unwrap();
        let steps = Mutex::new(Vec::new());
        let err = play_with("aws", &m.id, &|u| steps.lock().unwrap().push(u.step))
            .await
            .unwrap_err();
        assert!(err.message.contains("demo machine"), "{}", err.message);
        assert!(matches!(steps.lock().unwrap()[..], [PlayStep::Connecting]));
        assert!(stream_status().is_none());
        stop_machine("aws".into(), m.id.clone()).await.unwrap();

        let err = play_with("nope", &m.id, &|_| {}).await.unwrap_err();
        assert_eq!(err.message, "unknown provider nope");
        let err = pair_with_pin("aws".into(), m.id, "1234".into())
            .await
            .unwrap_err();
        assert!(err.message.contains("press Play first"), "{}", err.message);
        close_tunnel().await;
    }
}
