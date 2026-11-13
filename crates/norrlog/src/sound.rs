//! Alert sounds and speech.
//!
//! Sounds are synthesised: one oscillator per tone
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

/// `[frequency, seconds, waveform, ramp-to]` for each sound.
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
    let Some((f0, len, wave, f1)) = recipe(s) else {
        return Vec::new();
    };
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
                        let buf = rodio::buffer::SamplesBuffer::new(
                            NonZero::new(1u16).unwrap(),
                            NonZero::new(RATE).unwrap(),
                            samples,
                        );
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

/// A speech command: the program, its arguments, and the text, which on
/// Windows travels in an environment variable so that nothing in it is ever
/// read as PowerShell.
#[derive(Debug, PartialEq)]
struct Speech {
    program: &'static str,
    args: Vec<String>,
    env: Option<(&'static str, String)>,
}

/// The PowerShell that speaks `$env:NORRLOG_SAY`, with `{volume}` (0..100).
const SAPI_SPEAK: &str = "Add-Type -AssemblyName System.Speech; \
    $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
    $s.Volume = {volume}; $s.Rate = 2; $s.Speak($env:NORRLOG_SAY)";
const SAPI_VOICES: &str = "Add-Type -AssemblyName System.Speech; \
    (New-Object System.Speech.Synthesis.SpeechSynthesizer).GetInstalledVoices().Count";

/// How `os` says `text` at `gain` (0..1), slightly fast: an alert is worth
/// hearing now, not narrated. `None` for nothing to say, or no gain, which
/// is silence everywhere.
fn speech(os: &str, text: &str, gain: f32) -> Option<Speech> {
    if text.trim().is_empty() || gain <= 0. {
        return None;
    }
    let gain = gain.min(1.);
    Some(match os {
        // `say` has no volume flag; its embedded `volm` command is the
        // per-utterance volume, 0..1.
        "macos" => Speech {
            program: "say",
            args: vec![
                "-r".into(),
                "200".into(),
                "--".into(),
                format!("[[volm {gain:.2}]] {text}"),
            ],
            env: None,
        },
        "windows" => Speech {
            program: "powershell",
            args: ["-NoProfile", "-NonInteractive", "-Command"]
                .into_iter()
                .map(String::from)
                .chain(
                    [SAPI_SPEAK.replace("{volume}", &((gain * 100.).round() as u32).to_string())],
                )
                .collect(),
            env: Some(("NORRLOG_SAY", text.to_string())),
        },
        // speech-dispatcher: volume runs -100..100 with 0 as the default.
        _ => Speech {
            program: "spd-say",
            args: vec![
                "-r".into(),
                "10".into(),
                "-i".into(),
                (((gain * 200.) - 100.).round() as i32).to_string(),
                "--".into(),
                text.to_string(),
            ],
            env: None,
        },
    })
}

fn command(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window flashing up over the game.
        c.creation_flags(0x0800_0000);
    }
    c
}

/// Say `text` at `gain` (0..1). The process is waited for on a thread of its
/// own, so none is left behind, and a failure to start it is said rather
/// than swallowed.
pub fn speak(text: &str, gain: f32) -> Result<(), String> {
    let Some(s) = speech(std::env::consts::OS, text, gain) else {
        return Ok(());
    };
    let mut c = command(s.program);
    c.args(&s.args);
    if let Some((k, v)) = &s.env {
        c.env(k, v);
    }
    let mut child = c
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            let why = format!("could not run {}: {e}", s.program);
            eprintln!("[norrlog] speech: {why}");
            why
        })?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Whether the system can speak at all. speech-dispatcher accepts text with
/// no voice behind it and says nothing, which looks like a broken alert, so
/// its voice list is read: `dummy` alone is no voice. Windows is asked how
/// many voices are installed.
pub fn voices_available() -> bool {
    match std::env::consts::OS {
        "macos" => true,
        "windows" => command("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", SAPI_VOICES])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<u32>()
                    .ok()
            })
            .is_some_and(|n| n > 0),
        _ => {
            let Ok(out) = command("spd-say").arg("-L").output() else {
                return false;
            };
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .skip(1)
                .any(|l| {
                    let name = l.split_whitespace().next().unwrap_or("");
                    !name.is_empty() && name != "dummy"
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RATE, speech, synth};

    #[test]
    fn each_system_is_asked_to_speak_at_the_gain_given() {
        for os in ["linux", "macos", "windows"] {
            assert_eq!(
                speech(os, "charm broke", 0.),
                None,
                "{os}: no gain is silence"
            );
            assert_eq!(speech(os, "  ", 1.), None);
        }
        let mac = |g| speech("macos", "charm broke", g).unwrap().args[3].clone();
        assert_eq!(mac(0.1), "[[volm 0.10]] charm broke");
        assert_eq!(mac(1.0), "[[volm 1.00]] charm broke");
        let win = speech("windows", "it's \"$(rm -rf)\"", 0.25).unwrap();
        assert_eq!(win.program, "powershell");
        assert!(win.args.last().unwrap().contains("$s.Volume = 25;"));
        assert!(
            !win.args.iter().any(|a| a.contains("rm -rf")),
            "the text is never part of the script"
        );
        assert_eq!(
            win.env,
            Some(("NORRLOG_SAY", "it's \"$(rm -rf)\"".to_string()))
        );
        let lin = speech("linux", "charm broke", 0.5).unwrap();
        assert_eq!((lin.program, lin.args[3].as_str()), ("spd-say", "0"));
    }
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
