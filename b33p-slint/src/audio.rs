//! Playing audio and voice messages in the window. One plays at a time; starting another
//! stops it. The sound device is opened for each, so one plugged in since is used.

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use b33p_core::AttachmentId;
use rodio::Source as _;

#[derive(Default)]
pub struct Player {
    current: Option<Playing>,
}

struct Playing {
    id: AttachmentId,
    player: rodio::Player,
    length: Option<Duration>,
    /// Open while this plays: dropping it silences it.
    _device: rodio::MixerDeviceSink,
}

/// What is playing, for the UI.
pub struct State<'a> {
    pub id: &'a AttachmentId,
    pub paused: bool,
    pub position: Duration,
    pub length: Option<Duration>,
}

impl Player {
    /// Plays `audio` (a downloaded attachment) from the start.
    pub fn play(&mut self, id: AttachmentId, audio: Arc<[u8]>) -> Result<(), String> {
        self.stop();
        let decoder = decoder(audio).map_err(|e| format!("this audio can't be played: {e}"))?;
        let mut device = rodio::DeviceSinkBuilder::open_default_sink()
            .map_err(|e| format!("no sound device: {e}"))?;
        device.log_on_drop(false);
        let length = decoder.total_duration();
        let player = rodio::Player::connect_new(device.mixer());
        player.append(decoder);
        self.current = Some(Playing {
            id,
            player,
            length,
            _device: device,
        });
        Ok(())
    }

    /// Pauses, or carries on.
    pub fn toggle(&mut self) {
        if let Some(playing) = &self.current {
            if playing.player.is_paused() {
                playing.player.play();
            } else {
                playing.player.pause();
            }
        }
    }

    /// Goes to `fraction` (0 to 1) of what plays, if it is `id` and its length is known.
    pub fn seek(&mut self, id: &AttachmentId, fraction: f32) -> Result<(), String> {
        let Some(playing) = self.current.as_ref().filter(|p| &p.id == id) else {
            return Ok(());
        };
        let Some(length) = playing.length else {
            return Ok(());
        };
        let at = length.mul_f32(fraction.clamp(0.0, 1.0));
        playing
            .player
            .try_seek(at)
            .map_err(|e| format!("can't skip in this audio: {e}"))
    }

    pub fn stop(&mut self) {
        if let Some(playing) = self.current.take() {
            playing.player.stop();
        }
    }

    /// What is playing; forgets it once it has ended.
    pub fn state(&mut self) -> Option<State<'_>> {
        if self.current.as_ref().is_some_and(|p| p.player.empty()) {
            self.current = None;
        }
        let playing = self.current.as_ref()?;
        Some(State {
            id: &playing.id,
            paused: playing.player.is_paused(),
            position: playing.player.get_pos(),
            length: playing.length,
        })
    }
}

/// A decoder for audio in memory. Its length and seekability let MP4 (which keeps its index
/// at the end) be read, and the duration be known.
fn decoder(
    audio: Arc<[u8]>,
) -> Result<rodio::Decoder<Cursor<Arc<[u8]>>>, rodio::decoder::DecoderError> {
    let length = audio.len() as u64;
    rodio::Decoder::builder()
        .with_data(Cursor::new(audio))
        .with_byte_len(length)
        .with_seekable(true)
        .build()
}

/// "0:07" or "1:02:03".
pub fn clock(time: Duration) -> String {
    let seconds = time.as_secs();
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decoding needs no sound device: the mock's voice message decodes, with its length.
    #[test]
    fn voice_messages_decode() {
        let audio: Arc<[u8]> = b33p_core::mock_voice_message().into();
        let decoder = decoder(audio).expect("a WAV file");
        let length = decoder.total_duration().expect("its length is known");
        assert_eq!(length.as_secs(), 2);
    }

    /// Signal's voice messages are AAC, raw (ADTS) or in MP4. Run with
    /// `B33P_AUDIO_SAMPLES=<dir with voice.aac and voice.m4a> cargo test -- --ignored`
    /// (e.g. `ffmpeg -f lavfi -i sine=duration=3 -c:a aac -f adts voice.aac`).
    #[test]
    #[ignore]
    fn aac_voice_messages_decode() {
        let dir = std::env::var("B33P_AUDIO_SAMPLES").expect("B33P_AUDIO_SAMPLES");
        for name in ["voice.aac", "voice.m4a"] {
            let audio: Arc<[u8]> = std::fs::read(format!("{dir}/{name}")).unwrap().into();
            let samples = decoder(audio)
                .unwrap_or_else(|e| panic!("{name}: {e}"))
                .count();
            assert!(samples > 8000, "{name} has sound");
        }
    }

    #[test]
    fn clocks_read_naturally() {
        assert_eq!(clock(Duration::from_secs(7)), "0:07");
        assert_eq!(clock(Duration::from_secs(62)), "1:02");
        assert_eq!(clock(Duration::from_secs(3723)), "1:02:03");
    }
}
