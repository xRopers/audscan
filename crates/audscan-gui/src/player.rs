//! Playing decoded audio. On Windows this goes through the waveOut API (winmm), which is
//! part of Windows, so audscan needs no audio library; elsewhere playback isn't available
//! and [`Player::play`] says so.
//!
//! Playback runs on its own thread, feeding the device ~100 ms buffers, and reports the
//! position it has reached. More than two channels are mixed down to stereo.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

use audscan_core::Pcm;

#[derive(Default)]
pub struct Player {
    current: Option<Playing>,
}

struct Playing {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    /// Frames played from the start of the sound.
    position: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    /// Play `pcm` from frame `from`, stopping anything already playing.
    pub fn play(&mut self, pcm: Arc<Pcm>, from: u64) -> Result<(), String> {
        self.stop();
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let position = Arc::new(AtomicU64::new(from));
        let (tx, rx) = std::sync::mpsc::channel();
        let (s, d, p) = (stop.clone(), done.clone(), position.clone());
        let thread = std::thread::spawn(move || {
            backend::run(&pcm, from, &s, &p, |started| {
                let _ = tx.send(started);
            });
            d.store(true, Ordering::Release);
        });
        match rx.recv() {
            Ok(Ok(())) => {
                self.current = Some(Playing { stop, done, position, thread: Some(thread) });
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("playback stopped unexpectedly".into()),
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut p) = self.current.take() {
            p.stop.store(true, Ordering::Release);
            if let Some(t) = p.thread.take() {
                let _ = t.join();
            }
        }
    }

    pub fn playing(&self) -> bool {
        self.current.as_ref().is_some_and(|p| !p.done.load(Ordering::Acquire))
    }

    /// The frame reached, while playing.
    pub fn position(&self) -> Option<u64> {
        self.current.as_ref().filter(|_| self.playing()).map(|p| p.position.load(Ordering::Acquire))
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Interleaved 16-bit samples from frame `from`, mixed down to stereo if there are more
/// than two channels. Returns them and their channel count.
fn playable(pcm: &Pcm, from: u64) -> (Vec<i16>, u16) {
    let channels = usize::from(pcm.channels.max(1));
    let start = (from as usize * channels).min(pcm.samples.len());
    let samples = &pcm.samples[start..];
    if channels <= 2 {
        return (samples.to_vec(), channels as u16);
    }
    // Even channels to the left, odd to the right, averaged.
    let mixed = samples
        .chunks_exact(channels)
        .flat_map(|frame| {
            let side = |parity: usize| {
                let (sum, n) = frame.iter().skip(parity).step_by(2).fold((0i32, 0i32), |(s, n), &v| (s + i32::from(v), n + 1));
                (sum / n.max(1)) as i16
            };
            [side(0), side(1)]
        })
        .collect();
    (mixed, 2)
}

#[cfg(windows)]
mod backend {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    use audscan_core::Pcm;
    use windows_sys::Win32::Media::Audio::{
        CALLBACK_NULL, HWAVEOUT, WAVE_FORMAT_PCM, WAVE_MAPPER, WAVEFORMATEX, WAVEHDR, WHDR_DONE, waveOutClose, waveOutGetPosition,
        waveOutOpen, waveOutPrepareHeader, waveOutReset, waveOutUnprepareHeader, waveOutWrite,
    };
    use windows_sys::Win32::Media::{MMTIME, TIME_SAMPLES};

    const BUFFERS: usize = 6;

    pub fn run(pcm: &Pcm, from: u64, stop: &AtomicBool, position: &AtomicU64, started: impl FnOnce(Result<(), String>)) {
        let (samples, channels) = super::playable(pcm, from);
        let block = u32::from(channels) * 2;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: channels,
            nSamplesPerSec: pcm.sample_rate,
            nAvgBytesPerSec: pcm.sample_rate * block,
            nBlockAlign: block as u16,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut device: HWAVEOUT = std::ptr::null_mut();
        // SAFETY: `format` and `device` outlive the call; no callback is used.
        let result = unsafe { waveOutOpen(&mut device, WAVE_MAPPER, &format, 0, 0, CALLBACK_NULL) };
        if result != 0 {
            started(Err(format!("the audio device couldn't be opened (waveOut error {result})")));
            return;
        }
        started(Ok(()));

        let chunk = (pcm.sample_rate as usize / 10).max(1) * usize::from(channels);
        let chunks: Vec<&[i16]> = samples.chunks(chunk).collect();
        let mut headers: Vec<WAVEHDR> = (0..BUFFERS).map(|_| WAVEHDR::default()).collect();
        let mut prepared = [false; BUFFERS];
        let size = size_of::<WAVEHDR>() as u32;
        let mut next = 0;
        loop {
            if stop.load(Ordering::Acquire) {
                break;
            }
            let mut busy = false;
            for (i, header) in headers.iter_mut().enumerate() {
                // The driver sets WHDR_DONE from its own thread.
                // SAFETY: a plain read of a field of a live header.
                let flags = unsafe { std::ptr::read_volatile(&raw const header.dwFlags) };
                if prepared[i] && flags & WHDR_DONE == 0 {
                    busy = true;
                    continue;
                }
                if prepared[i] {
                    // SAFETY: the header was prepared on this device and is done.
                    unsafe { waveOutUnprepareHeader(device, header, size) };
                    prepared[i] = false;
                }
                if let Some(data) = chunks.get(next) {
                    *header = WAVEHDR {
                        lpData: data.as_ptr() as *mut u8,
                        dwBufferLength: (data.len() * 2) as u32,
                        ..WAVEHDR::default()
                    };
                    // SAFETY: `data` lives in `samples` until the device is closed below.
                    unsafe {
                        waveOutPrepareHeader(device, header, size);
                        waveOutWrite(device, header, size);
                    }
                    prepared[i] = true;
                    busy = true;
                    next += 1;
                }
            }
            let mut time = MMTIME { wType: TIME_SAMPLES, ..MMTIME::default() };
            // SAFETY: `time` is a valid MMTIME of the size given.
            if unsafe { waveOutGetPosition(device, &mut time, size_of::<MMTIME>() as u32) } == 0 && time.wType == TIME_SAMPLES {
                // SAFETY: wType says the union holds samples.
                position.store(from + u64::from(unsafe { time.u.sample }), Ordering::Release);
            }
            if !busy {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // SAFETY: reset returns every buffer to us; then each prepared header is released and
        // the device closed, all before `samples` is dropped.
        unsafe {
            waveOutReset(device);
            for (i, header) in headers.iter_mut().enumerate() {
                if prepared[i] {
                    waveOutUnprepareHeader(device, header, size);
                }
            }
            waveOutClose(device);
        }
    }
}

#[cfg(not(windows))]
mod backend {
    use std::sync::atomic::{AtomicBool, AtomicU64};

    use audscan_core::Pcm;

    pub fn run(_: &Pcm, _: u64, _: &AtomicBool, _: &AtomicU64, started: impl FnOnce(Result<(), String>)) {
        started(Err("playback is only available on Windows".into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surround_is_mixed_down_to_stereo() {
        let pcm = Pcm { channels: 4, sample_rate: 8000, samples: vec![100, 10, 300, 30, 1, 2, 3, 4] };
        assert_eq!(playable(&pcm, 0), (vec![200, 20, 2, 3], 2));
        assert_eq!(playable(&pcm, 1), (vec![2, 3], 2));
        let stereo = Pcm { channels: 2, sample_rate: 8000, samples: vec![1, 2, 3, 4] };
        assert_eq!(playable(&stereo, 1), (vec![3, 4], 2));
    }
}

/// Plays a real sound, so it needs an audio device: `cargo test -p audscan-gui -- --ignored`.
#[cfg(test)]
mod device_tests {
    use super::*;

    #[test]
    #[ignore = "needs an audio device"]
    fn a_tone_plays_and_the_position_moves() {
        // Half a second of a quiet 440 Hz tone.
        let samples = (0..22050).map(|i| ((i as f32 * 440.0 * std::f32::consts::TAU / 44100.0).sin() * 2000.0) as i16).collect();
        let pcm = Arc::new(Pcm { channels: 1, sample_rate: 44100, samples });
        let mut player = Player::default();
        player.play(pcm, 0).expect("the device opens");
        std::thread::sleep(std::time::Duration::from_millis(250));
        let halfway = player.position().expect("still playing");
        assert!(halfway > 2000 && halfway < 22050, "{halfway}");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while player.playing() {
            assert!(std::time::Instant::now() < deadline, "didn't finish");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // Stopping part way through works too.
        let pcm = Arc::new(Pcm { channels: 6, sample_rate: 48000, samples: vec![0; 6 * 48000] });
        player.play(pcm, 1000).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        player.stop();
        assert!(!player.playing());
    }
}
