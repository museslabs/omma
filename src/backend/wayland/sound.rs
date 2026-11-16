//! sounds. Tink and Glass are made here, a light tick and a glassy chime, so the defaults
//! need no sound theme. any other sound is a file that the config found. PipeWire, PulseAudio
//! or ALSA plays it, whichever has its player installed.

use std::f64::consts::TAU;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

const RATE: u32 = 48_000;

/// (frequency in Hz, loudness, decay time in seconds) of each partial
type Partials = &'static [(f64, f64, f64)];

/// a short, bright tick, like a fingernail on glass
const TINK: Partials = &[
    (2093.0, 1.0, 0.045),
    (4198.0, 0.35, 0.030),
    (6279.0, 0.15, 0.020),
];
/// a glass struck once: inharmonic partials that die away at their own rates
const GLASS: Partials = &[
    (1318.5, 1.0, 0.55),
    (2642.0, 0.50, 0.35),
    (3943.0, 0.28, 0.22),
    (5274.0, 0.16, 0.14),
    (7902.0, 0.06, 0.07),
];

/// players in the order that omma tries them. aplay reads only wav files, and the built-in
/// sounds are wav files
const PLAYERS: [(&str, &[&str]); 3] = [("pw-play", &[]), ("paplay", &[]), ("aplay", &["-q"])];

/// plays `sound`, a built-in sound's name or a file's path, without waiting for it
pub fn play(sound: &str) {
    let file = match built_in(sound) {
        Some(partials) => match written(sound, partials) {
            Some(path) => path,
            None => return,
        },
        None => PathBuf::from(sound),
    };
    // players run to the end of the sound, so a thread waits for each
    let spawned = std::thread::Builder::new()
        .name("omma-sound".into())
        .spawn(move || {
            for (player, args) in PLAYERS {
                let child = Command::new(player)
                    .args(args)
                    .arg(&file)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                match child {
                    Ok(mut child) => {
                        if child.wait().is_ok_and(|status| status.success()) {
                            return;
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => {
                        log!("can't play {}: {e}", file.display());
                        return;
                    }
                }
            }
            log!(
                "can't play {}: install pw-play, paplay or aplay",
                file.display()
            );
        });
    if let Err(e) = spawned {
        log!("can't play {sound}: {e}");
    }
}

fn built_in(name: &str) -> Option<Partials> {
    match name {
        "Tink" => Some(TINK),
        "Glass" => Some(GLASS),
        _ => None,
    }
}

/// the built-in sound's wav file, written once per run into the runtime directory
fn written(name: &str, partials: Partials) -> Option<PathBuf> {
    static TINK_FILE: OnceLock<Option<PathBuf>> = OnceLock::new();
    static GLASS_FILE: OnceLock<Option<PathBuf>> = OnceLock::new();
    let cell = if name == "Tink" {
        &TINK_FILE
    } else {
        &GLASS_FILE
    };
    cell.get_or_init(|| {
        let dir = crate::ipc::runtime_dir()?;
        let path = dir.join(format!("{name}.wav"));
        let length = partials
            .iter()
            .map(|&(_, _, decay)| decay)
            .fold(0.0, f64::max)
            * 7.0;
        match std::fs::write(&path, wav(&synthesize(partials, length))) {
            Ok(()) => Some(path),
            Err(e) => {
                log!("can't write {}: {e}", path.display());
                None
            }
        }
    })
    .clone()
}

/// `seconds` of sound from `partials`. each partial starts at once with a 2 ms rise and dies
/// away exponentially. the sound peaks well under full scale
fn synthesize(partials: Partials, seconds: f64) -> Vec<i16> {
    let total: f64 = partials.iter().map(|&(_, loud, _)| loud).sum();
    let count = (seconds * f64::from(RATE)) as usize;
    (0..count)
        .map(|i| {
            let t = i as f64 / f64::from(RATE);
            let rise = (t / 0.002).min(1.0);
            let sample: f64 = partials
                .iter()
                .map(|&(freq, loud, decay)| loud * (TAU * freq * t).sin() * (-t / decay).exp())
                .sum();
            (sample / total * rise * 0.45 * f64::from(i16::MAX)) as i16
        })
        .collect()
}

/// 16-bit mono PCM in a wav container
fn wav(samples: &[i16]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BUILT_IN_SOUNDS;

    #[test]
    fn every_built_in_sound_the_config_allows_is_made_here() {
        for name in BUILT_IN_SOUNDS {
            assert!(built_in(name).is_some(), "{name}");
        }
    }

    #[test]
    fn a_sound_rises_peaks_under_full_scale_and_dies_away() {
        let samples = synthesize(GLASS, 4.0);
        assert_eq!(samples.len(), 4 * RATE as usize);
        assert_eq!(samples[0], 0);
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > i16::MAX as u16 / 4 && peak < i16::MAX as u16 / 2);
        let tail = samples[samples.len() - 100..]
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .unwrap();
        assert!(tail < peak / 50);
    }

    #[test]
    fn a_wav_header_says_what_follows() {
        let bytes = wav(&[0, 1, -1]);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 6);
        assert_eq!(bytes.len(), 44 + 6);
    }
}
