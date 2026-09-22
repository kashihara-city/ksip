//! Reads a RIFF/WAVE file and rewrites it as 16-bit PCM, which is the only
//! shape baresip's file player accepts for call sounds.
use crate::message::{message, message_with};
const MAX_INPUT: usize = 16 * 1024 * 1024;

struct Format {
    encoding: u16,
    channels: u16,
    rate: u32,
    bits: u16,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn parse(bytes: &[u8]) -> Result<(Format, &[u8]), String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(message("WAV_NOT_WAV"));
    }
    let mut format: Option<Format> = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32_at(bytes, at + 4) as usize;
        let body = at + 8;
        let end = body.checked_add(size).ok_or(message("WAV_BROKEN"))?;
        if end > bytes.len() {
            return Err(message("WAV_BROKEN"));
        }
        if id == b"fmt " && size >= 16 {
            let mut encoding = u16_at(bytes, body);
            // WAVE_FORMAT_EXTENSIBLE keeps the real encoding in the sub-format.
            if encoding == 0xFFFE && size >= 40 {
                encoding = u16_at(bytes, body + 24);
            }
            format = Some(Format {
                encoding,
                channels: u16_at(bytes, body + 2),
                rate: u32_at(bytes, body + 4),
                bits: u16_at(bytes, body + 14),
            });
        } else if id == b"data" {
            let format = format.ok_or(message("WAV_NO_FMT"))?;
            return Ok((format, &bytes[body..end]));
        }
        at = end + (size & 1);
    }
    Err(message("WAV_NO_DATA"))
}

fn samples(format: &Format, data: &[u8]) -> Result<Vec<i16>, String> {
    let width = (format.bits / 8) as usize;
    if width == 0 || data.len() < width {
        return Err(message("WAV_EMPTY"));
    }
    let count = data.len() / width;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = i * width;
        let value = match (format.encoding, format.bits) {
            (1, 8) => (i16::from(data[at]) - 128) << 8,
            (1, 16) => i16::from_le_bytes([data[at], data[at + 1]]),
            (1, 24) => i16::from_le_bytes([data[at + 1], data[at + 2]]),
            (1, 32) => (i32::from_le_bytes([
                data[at],
                data[at + 1],
                data[at + 2],
                data[at + 3],
            ]) >> 16) as i16,
            (3, 32) => {
                let f = f32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
                (f.clamp(-1.0, 1.0) * 32767.0) as i16
            }
            (3, 64) => {
                let mut raw = [0u8; 8];
                raw.copy_from_slice(&data[at..at + 8]);
                (f64::from_le_bytes(raw).clamp(-1.0, 1.0) * 32767.0) as i16
            }
            _ => {
                return Err(message_with(
                    "WAV_FORMAT_UNSUPPORTED",
                    [format.encoding, format.bits],
                ))
            }
        };
        out.push(value);
    }
    Ok(out)
}

/// Converts any supported WAV into 16-bit PCM with the original rate and channels.
pub fn to_pcm16(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() > MAX_INPUT {
        return Err(message("WAV_TOO_LARGE"));
    }
    let (format, data) = parse(bytes)?;
    if format.channels == 0 || format.rate == 0 {
        return Err(message("WAV_HEADER_INVALID"));
    }
    let samples = samples(&format, data)?;
    let payload = samples.len() * 2;
    let mut out = Vec::with_capacity(44 + payload);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + payload) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&format.channels.to_le_bytes());
    out.extend_from_slice(&format.rate.to_le_bytes());
    let block = u32::from(format.channels) * 2;
    // A rate large enough to overflow here describes no sound anyone recorded.
    let bytes_per_second = format
        .rate
        .checked_mul(block)
        .ok_or(message("WAV_HEADER_INVALID"))?;
    out.extend_from_slice(&bytes_per_second.to_le_bytes());
    out.extend_from_slice(&(block as u16).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(payload as u32).to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(encoding: u16, bits: u16, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&encoding.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&8000u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    fn payload(converted: &[u8]) -> Vec<i16> {
        converted[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c))
            .collect()
    }

    #[test]
    fn converts_the_formats_a_person_is_likely_to_pick() {
        let pcm16 = wav(1, 16, &[0x00, 0x80, 0xFF, 0x7F]);
        assert_eq!(payload(&to_pcm16(&pcm16).unwrap()), vec![-32768, 32767]);
        let float32 = wav(3, 32, &1.0f32.to_le_bytes());
        assert_eq!(payload(&to_pcm16(&float32).unwrap()), vec![32767]);
        let pcm24 = wav(1, 24, &[0x00, 0x00, 0x40]);
        assert_eq!(payload(&to_pcm16(&pcm24).unwrap()), vec![16384]);
        let pcm8 = wav(1, 8, &[0x80, 0xFF]);
        assert_eq!(payload(&to_pcm16(&pcm8).unwrap()), vec![0, 32512]);
        // The header must describe 16-bit PCM whatever went in.
        let header = to_pcm16(&float32).unwrap();
        assert_eq!(u16_at(&header, 20), 1);
        assert_eq!(u16_at(&header, 34), 16);
        assert_eq!(u32_at(&header, 24), 8000);
    }

    #[test]
    fn refuses_what_it_cannot_turn_into_sound() {
        assert!(to_pcm16(b"not a wav at all").is_err());
        let mut absurd = wav(1, 16, &[0x00, 0x80]);
        absurd[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(to_pcm16(&absurd).is_err(), "a rate that overflows the header is refused");
        assert!(to_pcm16(&wav(2, 4, &[0x11])).is_err(), "ADPCM is not decoded");
        assert!(to_pcm16(&vec![0u8; MAX_INPUT + 1]).is_err());
    }
}
