//! Alert sounds and speech: `playSound` and `speak` (alerts.js).
//!
//! Sounds are synthesised, as the renderer's are: one oscillator per tone
//! from the same recipes, with the same envelope. They play on a thread of
//! their own that opens the default output the first time it is asked to,
//! so a machine with no audio device costs one log line and nothing else.
//!
//! Speech goes to the system's own speaker: speech-dispatcher's `spd-say` on
//! Linux, `say` on macOS. Neither ships with the app.

use norrlog_api::Sound;
use std::num::NonZero;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

const RATE: u32 = 44_100;

#[derive(Clone, Copy)]
enum Wave {
    Sine,
    Triangle,
    Square,
    Sawtooth,
}

/// `[frequency, seconds, waveform, ramp-to]`, as `SOUNDS` in alerts.js.
fn recipe(s: Sound) -> Option<(f32, f32, Wave, f32)> {
    Some(match s {
        Sound::Chime => (880., 0.18, Wave::Sine, 1320.),
        Sound::Bell => (1320., 0.5, Wave::Triangle, 1320.),
        Sound::Alarm => (740., 0.5, Wave::Square, 500.),
        Sound::Buzz => (180., 0.35, Wave::Sawtooth, 140.),
        Sound::Blip => (1600., 0.07, Wave::Square, 1600.),
        Sound::Thud => (110., 0.22, Wave::Sine, 60.),
        Sound::None => return None,
    })
}

/// One tone's samples, mono: the pitch ramps exponentially from its start
/// to its end, and the level rises over 12 ms to `gain × 0.35` and decays
/// exponentially to nothing, so the tone never stops on a click.
pub fn synth(s: Sound, gain: f32) -> Vec<f32> {
    let Some((f0, len, wave, f1)) = recipe(s) else { return Vec::new() };
    if gain <= 0. {
        return Vec::new();
    }
    let n = (len * RATE as f32) as usize;
    let tail = (0.02 * RATE as f32) as usize;
    let attack = 0.012;
    let (floor, peak) = (0.0001f32, gain * 0.35);
    let mut out = Vec::with_capacity(n + tail);
    let mut phase = 0f32;
    for i in 0..n {
        let t = i as f32 / RATE as f32;
        let freq = f0 * (f1 / f0).powf(t / len);
        phase = (phase + freq / RATE as f32).fract();
        let v = match wave {
            Wave::Sine => (phase * std::f32::consts::TAU).sin(),
            Wave::Triangle => 1. - 4. * (phase - 0.5).abs(),
            Wave::Square => {
                if phase < 0.5 {
                    1.
                } else {
                    -1.
                }
            }
            Wave::Sawtooth => 2. * phase - 1.,
        };
        let env = if t < attack {
            floor * (peak / floor).powf(t / attack)
        } else {
            peak * (floor / peak).powf((t - attack) / (len - attack))
        };
        out.push(v * env);
    }
    out.extend(std::iter::repeat_n(0., tail));
    out
}

fn player() -> &'static Sender<Vec<f32>> {
    static TX: OnceLock<Sender<Vec<f32>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = channel::<Vec<f32>>();
        std::thread::Builder::new()
            .name("norrlog-sound".into())
            .spawn(move || {
                let mut sink = None;
                let mut failed = false;
                while let Ok(samples) = rx.recv() {
                    if sink.is_none() && !failed {
                        match rodio::DeviceSinkBuilder::open_default_sink() {
                            Ok(mut s) => {
                                s.log_on_drop(false);
                                sink = Some(s);
                            }
                            Err(e) => {
                                eprintln!("[norrlog] no audio output, alerts will be silent: {e}");
                                failed = true;
                            }
                        }
                    }
                    if let Some(s) = &sink {
                        let buf = rodio::buffer::SamplesBuffer::new(NonZero::new(1u16).unwrap(), NonZero::new(RATE).unwrap(), samples);
                        s.mixer().add(buf);
                    }
                }
            })
            .expect("sound thread");
        tx
    })
}

/// Play a sound at `gain` (0..1). Silent for `none` or no gain.
pub fn play(s: Sound, gain: f32) {
    let samples = synth(s, gain);
    if !samples.is_empty() {
        let _ = player().send(samples);
    }
}

/// Say `text` at `gain` (0..1), slightly fast: an alert is worth hearing
/// now, not narrated.
pub fn speak(text: &str, gain: f32) {
    if text.is_empty() || gain <= 0. {
        return;
    }
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("say");
        c.args(["-r", "200", "--", text]);
        c
    } else {
        let mut c = std::process::Command::new("spd-say");
        // Volume runs -100..100 with 0 as the default level.
        let volume = ((gain * 200.) - 100.).round() as i32;
        c.args(["-r", "10", "-i", &volume.to_string(), "--", text]);
        c
    };
    let _ = cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

/// Whether the system can speak at all. speech-dispatcher accepts text with
/// no voice behind it and says nothing, which looks like a broken alert, so
/// its voice list is read: `dummy` alone is no voice.
pub fn voices_available() -> bool {
    if cfg!(target_os = "macos") {
        return true;
    }
    let Ok(out) = std::process::Command::new("spd-say").arg("-L").output() else { return false };
    String::from_utf8_lossy(&out.stdout).lines().skip(1).any(|l| {
        let name = l.split_whitespace().next().unwrap_or("");
        !name.is_empty() && name != "dummy"
    })
}

#[cfg(test)]
mod tests {
    use super::{RATE, synth};
    use norrlog_api::Sound;

    #[test]
    fn a_tone_lasts_its_recipe_and_ends_near_silence() {
        let s = synth(Sound::Chime, 0.7);
        let tail = (0.02 * RATE as f32) as usize;
        assert_eq!(s.len(), (0.18 * RATE as f32) as usize + tail);
        let peak = s.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak <= 0.7 * 0.35 + 1e-4 && peak > 0.2, "peak {peak}");
        let body_end = s.len() - tail - 1;
        assert!(s[body_end].abs() < 0.001);
        assert!(synth(Sound::None, 1.).is_empty());
        assert!(synth(Sound::Bell, 0.).is_empty());
    }
}
