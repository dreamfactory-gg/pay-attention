use rodio::{Decoder, Source, buffer::SamplesBuffer};
use std::{
    fs::File,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

pub struct Sound {
    pub color: String,
    pub source: String,
    pub volume: f64,
    pub pitch: usize,
}

pub fn start() -> mpsc::Sender<Sound> {
    let (tx, rx) = mpsc::channel::<Sound>();
    thread::spawn(move || {
        let failed = Arc::new(AtomicBool::new(false));
        let mut stream = None;
        for sound in rx {
            if failed.swap(false, Ordering::SeqCst) {
                stream = None;
            }
            if stream.is_none() {
                let failed = failed.clone();
                match rodio::DeviceSinkBuilder::from_default_device().and_then(|builder| {
                    builder
                        .with_error_callback(move |error| {
                            eprintln!("audio: {error}");
                            failed.store(true, Ordering::SeqCst);
                        })
                        .open_sink_or_fallback()
                }) {
                    Ok(device) => stream = Some(device),
                    Err(error) => {
                        eprintln!("audio: {error}");
                        continue;
                    }
                }
            }
            let stream = stream.as_ref().unwrap();
            let path = if let Some(name) = sound.source.strip_prefix("system:") {
                PathBuf::from("/System/Library/Sounds").join(format!("{name}.aiff"))
            } else {
                PathBuf::from(&sound.source)
            };
            if sound.source != "synth"
                && let Ok(file) = File::open(&path)
                && let Ok(source) = Decoder::try_from(file)
            {
                stream.mixer().add(source.amplify(sound.volume as f32));
            } else {
                stream.mixer().add(synth(&sound));
            }
        }
    });
    tx
}

fn synth(sound: &Sound) -> SamplesBuffer {
    const RATE: u32 = 44_100;
    let notes: &[(f32, f32, f32)] = match sound.color.as_str() {
        "blue" => &[(0.0, 0.20, 659.25), (0.20, 0.30, 880.0)],
        "red" => &[(0.0, 0.13, 220.0), (0.18, 0.13, 220.0), (0.36, 0.13, 220.0)],
        _ => &[(0.0, 0.35, 440.0)],
    };
    let end = notes.last().unwrap().0 + notes.last().unwrap().1;
    let pitch = 2.0_f32.powf(sound.pitch.min(5) as f32 / 12.0);
    let samples = (0..(end * RATE as f32) as usize)
        .map(|i| {
            let time = i as f32 / RATE as f32;
            notes
                .iter()
                .map(|&(start, duration, frequency)| {
                    let t = time - start;
                    if !(0.0..duration).contains(&t) {
                        return 0.0;
                    }
                    let envelope = (t / 0.02).min(1.0) * ((duration - t) / 0.08).min(1.0);
                    (std::f32::consts::TAU * frequency * pitch * t).sin() * envelope
                })
                .sum::<f32>()
                * sound.volume as f32
                * 0.45
        })
        .collect::<Vec<_>>();
    SamplesBuffer::new(1.try_into().unwrap(), RATE.try_into().unwrap(), samples)
}

#[cfg(test)]
mod tests {
    #[test]
    fn system_aiff_decodes() {
        let file = std::fs::File::open("/System/Library/Sounds/Glass.aiff").unwrap();
        assert!(rodio::Decoder::try_from(file).is_ok());
    }
}
