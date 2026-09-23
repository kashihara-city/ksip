//! Turns a finished recording into an MP3 with the encoder Windows ships.
//!
//! Media Foundation reads the WAV, encodes with the MP3 encoder that has come
//! with Windows since 8, and writes the file; nothing is added to the
//! program. The one place it is missing is an "N" edition without the media
//! feature pack, and there the WAV simply stays.
use crate::message::message_with;
use std::path::Path;
use windows::core::HSTRING;
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSample, MFAudioFormat_MP3, MFAudioFormat_PCM, MFCreateMediaType,
    MFCreateSinkWriterFromURL, MFCreateSourceReaderFromURL, MFMediaType_Audio, MFShutdown, MFStartup,
    MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
    MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// Call audio at 64 kbit/s: a twelfth of the WAV, and every word intact.
const BITRATE: u32 = 64_000;

/// Writes `mp3` from `wav`. The caller decides what happens to the WAV.
pub fn transcode(wav: &Path, mp3: &Path) -> Result<(), String> {
    // SAFETY: Media Foundation is used on this thread alone, started and
    // stopped here, and every object it hands out is released by its wrapper.
    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED);
        let result = with_media_foundation(wav, mp3);
        if com.is_ok() {
            CoUninitialize();
        }
        result
    }
}

unsafe fn with_media_foundation(wav: &Path, mp3: &Path) -> Result<(), String> {
    let failed = |what: &str, e: windows::core::Error| message_with("RECORDING_CONVERT_FAILED", [format!("{what}: {e}")]);
    MFStartup(MF_VERSION, 0).map_err(|e| failed("start", e))?;
    let result = encode(wav, mp3, &failed);
    let _ = MFShutdown();
    result
}

unsafe fn encode(
    wav: &Path,
    mp3: &Path,
    failed: &dyn Fn(&str, windows::core::Error) -> String,
) -> Result<(), String> {
    let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
    let reader = MFCreateSourceReaderFromURL(&HSTRING::from(wav.as_os_str()), None).map_err(|e| failed("open", e))?;
    // The reader is asked for plain PCM, which is what a WAV holds anyway.
    let pcm: IMFMediaType = MFCreateMediaType().map_err(|e| failed("type", e))?;
    pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(|e| failed("type", e))?;
    pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).map_err(|e| failed("type", e))?;
    reader.SetCurrentMediaType(stream, None, &pcm).map_err(|e| failed("read type", e))?;
    let input = reader.GetCurrentMediaType(stream).map_err(|e| failed("read type", e))?;
    let rate = input.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).map_err(|e| failed("rate", e))?;
    let channels = input.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).map_err(|e| failed("channels", e))?;
    // The encoder takes 32, 44.1 and 48 kHz; a narrower call is brought up to
    // 48 kHz by the resampler the writer puts in front of the encoder.
    let out_rate = if matches!(rate, 32_000 | 44_100 | 48_000) { rate } else { 48_000 };
    let writer = MFCreateSinkWriterFromURL(&HSTRING::from(mp3.as_os_str()), None, None).map_err(|e| failed("create", e))?;
    let out: IMFMediaType = MFCreateMediaType().map_err(|e| failed("type", e))?;
    out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(|e| failed("type", e))?;
    out.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_MP3).map_err(|e| failed("type", e))?;
    out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, out_rate).map_err(|e| failed("type", e))?;
    out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels.min(2)).map_err(|e| failed("type", e))?;
    out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, BITRATE / 8).map_err(|e| failed("type", e))?;
    let index = writer.AddStream(&out).map_err(|e| failed("encoder", e))?;
    writer.SetInputMediaType(index, &input, None).map_err(|e| failed("input", e))?;
    writer.BeginWriting().map_err(|e| failed("begin", e))?;
    loop {
        let mut flags = 0u32;
        let mut sample: Option<IMFSample> = None;
        reader
            .ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))
            .map_err(|e| failed("read", e))?;
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            break;
        }
        if let Some(sample) = sample {
            writer.WriteSample(index, &sample).map_err(|e| failed("write", e))?;
        }
    }
    writer.Finalize().map_err(|e| failed("finish", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second of a tone, as the engine would have written it.
    fn wav(path: &Path, rate: u32) {
        let mut bytes = Vec::new();
        let samples: Vec<i16> = (0..rate)
            .map(|i| ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 12000.0) as i16)
            .collect();
        let data = samples.len() as u32 * 2;
        bytes.extend(b"RIFF");
        bytes.extend((36 + data).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * 2).to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(data.to_le_bytes());
        for sample in samples {
            bytes.extend(sample.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn a_wav_of_any_call_rate_becomes_a_much_smaller_mp3() {
        let dir = std::env::temp_dir().join(format!("ksip-mp3-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for rate in [48_000u32, 16_000, 8_000] {
            let source = dir.join(format!("{rate}.wav"));
            let target = dir.join(format!("{rate}.mp3"));
            wav(&source, rate);
            transcode(&source, &target).unwrap_or_else(|e| panic!("{rate} Hz: {e}"));
            // A second at 64 kbit/s is 8000 bytes, plus a little framing.
            let mp3 = std::fs::metadata(&target).unwrap().len();
            assert!((6_000..12_000).contains(&mp3), "{rate} Hz gave {mp3} bytes");
        }
        assert!(transcode(&dir.join("missing.wav"), &dir.join("missing.mp3")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
