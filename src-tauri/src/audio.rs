//! Windows endpoint controls and the persistent capture session used by the input meter.
use crate::message::{message, message_with};
use serde::{Deserialize, Serialize};
use std::{
    f32::consts::PI,
    sync::{mpsc, OnceLock},
    time::{Duration, Instant},
};
use windows::{
    core::{Interface, PCWSTR, PWSTR},
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Media::Audio::{
            eCapture, eCommunications, eRender,
            Endpoints::{IAudioEndpointVolume, IAudioMeterInformation},
            IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator,
            IMMEndpoint, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_NOPERSIST, DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
        },
        Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE},
        Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT},
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
            StructuredStorage::{PropVariantClear, PropVariantToStringAlloc, PROPVARIANT},
            CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
        },
    },
};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Volume {
    pub id: String,
    pub level: u16,
    pub muted: bool,
}
#[derive(Clone, Serialize)]
pub struct Peak {
    pub id: String,
    pub peak: f32,
}
#[derive(Clone, Serialize)]
pub struct Calibration {
    pub measured_ms: u16,
    pub recommended_ms: u16,
    pub confidence: u8,
    pub samples: u8,
    pub spread_ms: u16,
    pub stable: bool,
}
struct MicSession {
    requested_id: String,
    resolved_id: String,
    meter: IAudioMeterInformation,
    client: IAudioClient,
}
impl Drop for MicSession {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}
struct PeakRequest {
    device_id: String,
    reply: mpsc::SyncSender<Result<Peak, String>>,
}
struct Com;
impl Com {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
struct Property(PROPVARIANT);
impl Drop for Property {
    fn drop(&mut self) {
        unsafe {
            let _ = PropVariantClear(&mut self.0);
        }
    }
}
// GetId and PropVariantToStringAlloc return strings owned by the COM allocator.
unsafe fn take_string(value: PWSTR) -> windows::core::Result<String> {
    let result = value.to_string();
    CoTaskMemFree(Some(value.0.cast()));
    Ok(result?)
}
fn error(e: windows::core::Error) -> String {
    message_with("AUDIO_DEVICE_FAILED", [e])
}
unsafe fn open_microphone_session(device_id: &str) -> windows::core::Result<MicSession> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let wide: Vec<u16> = device_id.encode_utf16().chain(Some(0)).collect();
    let device = if device_id == "default" {
        enumerator.GetDefaultAudioEndpoint(eCapture, eCommunications)?
    } else {
        enumerator.GetDevice(PCWSTR(wide.as_ptr()))?
    };
    let endpoint: IMMEndpoint = device.cast()?;
    if endpoint.GetDataFlow()? != eCapture || device.GetState()?.0 & DEVICE_STATE_ACTIVE.0 == 0 {
        return Err(windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_INVALIDARG,
        ));
    }
    let meter: IAudioMeterInformation = device.Activate(CLSCTX_ALL, None)?;
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
    let format = client.GetMixFormat()?;
    let initialized = client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_NOPERSIST,
        0,
        0,
        format,
        None,
    );
    CoTaskMemFree(Some(format.cast()));
    initialized?;
    client.Start()?;
    std::thread::sleep(std::time::Duration::from_millis(40));
    Ok(MicSession {
        requested_id: device_id.into(),
        resolved_id: take_string(device.GetId()?)?,
        meter,
        client,
    })
}
fn microphone_peak(device_id: &str) -> Result<Peak, String> {
    static REQUESTS: OnceLock<mpsc::Sender<PeakRequest>> = OnceLock::new();
    let sender = REQUESTS.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<PeakRequest>();
        std::thread::spawn(move || {
            let _com = match Com::new() {
                Ok(com) => com,
                Err(e) => {
                    let message = error(e);
                    while let Ok(request) = rx.recv() {
                        let _ = request.reply.send(Err(message.clone()));
                    }
                    return;
                }
            };
            let mut session: Option<MicSession> = None;
            while let Ok(request) = rx.recv() {
                if session
                    .as_ref()
                    .is_none_or(|value| value.requested_id != request.device_id)
                {
                    session = unsafe { open_microphone_session(&request.device_id) }
                        .map_err(error)
                        .ok();
                }
                let result = match session.as_ref() {
                    Some(value) => unsafe {
                        value.meter.GetPeakValue().map(|peak| Peak {
                            id: value.resolved_id.clone(),
                            peak: peak.clamp(0.0, 1.0),
                        })
                    }
                    .map_err(error),
                    None => Err(message("AUDIO_MONITOR_START_FAILED")),
                };
                let _ = request.reply.send(result);
            }
        });
        tx
    });
    let (reply, response) = mpsc::sync_channel(1);
    sender
        .send(PeakRequest {
            device_id: device_id.into(),
            reply,
        })
        .map_err(|_| message("AUDIO_MONITOR_START_FAILED"))?;
    response
        .recv_timeout(std::time::Duration::from_secs(2))
        .map_err(|_| message("AUDIO_MONITOR_STALLED"))?
}
pub fn devices() -> Result<Vec<Device>, String> {
    fn run() -> windows::core::Result<Vec<Device>> {
        let _com = Com::new()?;
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let mut output = Vec::new();
            for (flow, kind) in [(eCapture, "microphone"), (eRender, "speaker")] {
                let devices = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
                for i in 0..devices.GetCount()? {
                    // A stale or partially removed endpoint can remain in the
                    // active collection. It must not prevent other devices or
                    // the SIP engine from starting.
                    let Ok(device) = devices.Item(i) else {
                        continue;
                    };
                    let Ok(raw_id) = device.GetId() else {
                        continue;
                    };
                    let Ok(id) = take_string(raw_id) else {
                        continue;
                    };
                    let name = (|| -> windows::core::Result<String> {
                        let properties = device.OpenPropertyStore(STGM_READ)?;
                        let value = Property(properties.GetValue(&PKEY_Device_FriendlyName)?);
                        take_string(PropVariantToStringAlloc(&value.0)?)
                    })()
                    .unwrap_or_else(|_| id.clone());
                    output.push(Device {
                        id,
                        name,
                        kind: kind.into(),
                    });
                }
            }
            Ok(output)
        }
    }
    run().map_err(error)
}
pub fn volume(kind: &str, device_id: &str, level: Option<u8>) -> Result<Volume, String> {
    if !matches!(kind, "microphone" | "speaker")
        || device_id.is_empty()
        || device_id.len() > 500
        || device_id.contains(['\r', '\n', '\0'])
        || level.is_some_and(|v| v > 100)
    {
        return Err(message("AUDIO_VOLUME_ARGUMENT_INVALID"));
    }
    fn run(kind: &str, device_id: &str, level: Option<u8>) -> windows::core::Result<Volume> {
        let _com = Com::new()?;
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let flow = if kind == "microphone" {
                eCapture
            } else {
                eRender
            };
            let wide: Vec<u16> = device_id.encode_utf16().chain(Some(0)).collect();
            let device = if device_id == "default" {
                enumerator.GetDefaultAudioEndpoint(flow, eCommunications)?
            } else {
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))?
            };
            let endpoint: IMMEndpoint = device.cast()?;
            if endpoint.GetDataFlow()? != flow || device.GetState()?.0 & DEVICE_STATE_ACTIVE.0 == 0
            {
                return Err(windows::core::Error::from_hresult(
                    windows::Win32::Foundation::E_INVALIDARG,
                ));
            }
            let control: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
            if let Some(level) = level {
                if let Err(set_error) =
                    control.SetMasterVolumeLevelScalar(level as f32 / 100.0, std::ptr::null())
                {
                    // Some USB microphone drivers apply the new level and then
                    // return ERROR_GEN_FAILURE. Accept that response only when
                    // an immediate readback proves that the requested value was
                    // actually applied; otherwise preserve the real failure.
                    let applied = (control.GetMasterVolumeLevelScalar()? * 100.0).round() as u8;
                    if applied.abs_diff(level) > 1 {
                        return Err(set_error);
                    }
                }
            }
            Ok(Volume {
                id: take_string(device.GetId()?)?,
                level: (control.GetMasterVolumeLevelScalar()? * 100.0).round() as u16,
                muted: control.GetMute()?.as_bool(),
            })
        }
    }
    run(kind, device_id, level).map_err(error)
}

pub fn peak(kind: &str, device_id: &str) -> Result<Peak, String> {
    if !matches!(kind, "microphone" | "speaker")
        || device_id.is_empty()
        || device_id.len() > 500
        || device_id.contains(['\r', '\n', '\0'])
    {
        return Err(message("AUDIO_DEVICE_ARGUMENT_INVALID"));
    }
    if kind == "microphone" {
        return microphone_peak(device_id);
    }
    fn run(kind: &str, device_id: &str) -> windows::core::Result<Peak> {
        let _com = Com::new()?;
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let flow = if kind == "microphone" {
                eCapture
            } else {
                eRender
            };
            let wide: Vec<u16> = device_id.encode_utf16().chain(Some(0)).collect();
            let device = if device_id == "default" {
                enumerator.GetDefaultAudioEndpoint(flow, eCommunications)?
            } else {
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))?
            };
            let endpoint: IMMEndpoint = device.cast()?;
            if endpoint.GetDataFlow()? != flow || device.GetState()?.0 & DEVICE_STATE_ACTIVE.0 == 0
            {
                return Err(windows::core::Error::from_hresult(
                    windows::Win32::Foundation::E_INVALIDARG,
                ));
            }
            let meter: IAudioMeterInformation = device.Activate(CLSCTX_ALL, None)?;
            Ok(Peak {
                id: take_string(device.GetId()?)?,
                peak: meter.GetPeakValue()?.clamp(0.0, 1.0),
            })
        }
    }
    run(kind, device_id).map_err(error)
}

#[derive(Clone, Copy)]
enum SampleKind {
    Float32,
    Pcm8,
    Pcm16,
    Pcm24,
    Pcm32,
}
#[derive(Clone, Copy)]
struct MixFormat {
    rate: u32,
    channels: u16,
    block: u16,
    kind: SampleKind,
}
unsafe fn mix_format(raw: *const WAVEFORMATEX) -> Result<MixFormat, String> {
    let base = unsafe { std::ptr::read_unaligned(raw) };
    let tag = base.wFormatTag as u32;
    let subformat = if tag == WAVE_FORMAT_EXTENSIBLE {
        let extended = raw.cast::<WAVEFORMATEXTENSIBLE>();
        Some(unsafe { std::ptr::read_unaligned(std::ptr::addr_of!((*extended).SubFormat)) })
    } else {
        None
    };
    let floating = tag == WAVE_FORMAT_IEEE_FLOAT
        || subformat.is_some_and(|value| value == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
    let pcm = tag == windows::Win32::Media::Audio::WAVE_FORMAT_PCM
        || subformat.is_some_and(|value| value == KSDATAFORMAT_SUBTYPE_PCM);
    let kind = match (floating, pcm, base.wBitsPerSample) {
        (true, _, 32) => SampleKind::Float32,
        (_, true, 8) => SampleKind::Pcm8,
        (_, true, 16) => SampleKind::Pcm16,
        (_, true, 24) => SampleKind::Pcm24,
        (_, true, 32) => SampleKind::Pcm32,
        _ => return Err(message("AUDIO_SAMPLE_FORMAT_UNSUPPORTED")),
    };
    if base.nSamplesPerSec < 8000 || base.nChannels == 0 || base.nBlockAlign == 0 {
        return Err(message("AUDIO_DEVICE_FORMAT_INVALID"));
    }
    Ok(MixFormat {
        rate: base.nSamplesPerSec,
        channels: base.nChannels,
        block: base.nBlockAlign,
        kind,
    })
}
unsafe fn write_sample(buffer: *mut u8, frame: usize, format: MixFormat, value: f32) {
    let bytes = format.block as usize / format.channels as usize;
    let value = value.clamp(-1.0, 1.0);
    for channel in 0..format.channels as usize {
        let p = unsafe { buffer.add(frame * format.block as usize + channel * bytes) };
        match format.kind {
            SampleKind::Float32 => unsafe { std::ptr::write_unaligned(p.cast(), value) },
            SampleKind::Pcm8 => unsafe { *p = ((value * 127.0) + 128.0).round() as u8 },
            SampleKind::Pcm16 => unsafe {
                std::ptr::write_unaligned(p.cast(), (value * 32767.0).round() as i16)
            },
            SampleKind::Pcm24 => {
                let sample = (value * 8_388_607.0).round() as i32;
                unsafe {
                    *p = sample as u8;
                    *p.add(1) = (sample >> 8) as u8;
                    *p.add(2) = (sample >> 16) as u8;
                }
            }
            SampleKind::Pcm32 => unsafe {
                std::ptr::write_unaligned(p.cast(), (value * 2_147_483_647.0).round() as i32)
            },
        }
    }
}
unsafe fn read_sample(buffer: *const u8, frame: usize, format: MixFormat) -> f32 {
    let bytes = format.block as usize / format.channels as usize;
    let mut total = 0.0;
    for channel in 0..format.channels as usize {
        let p = unsafe { buffer.add(frame * format.block as usize + channel * bytes) };
        total += match format.kind {
            SampleKind::Float32 => unsafe { std::ptr::read_unaligned(p.cast::<f32>()) },
            SampleKind::Pcm8 => unsafe { (*p as f32 - 128.0) / 128.0 },
            SampleKind::Pcm16 => unsafe {
                std::ptr::read_unaligned(p.cast::<i16>()) as f32 / 32768.0
            },
            SampleKind::Pcm24 => unsafe {
                let mut value = *p as i32 | ((*p.add(1) as i32) << 8) | ((*p.add(2) as i32) << 16);
                if value & 0x800000 != 0 {
                    value |= !0xffffff;
                }
                value as f32 / 8_388_608.0
            },
            SampleKind::Pcm32 => unsafe {
                std::ptr::read_unaligned(p.cast::<i32>()) as f32 / 2_147_483_648.0
            },
        };
    }
    total / format.channels as f32
}
fn probe(time: f32, variant: u8) -> f32 {
    const LENGTH: f32 = 0.35;
    if !(0.0..LENGTH).contains(&time) {
        return 0.0;
    }
    let fade = (time / 0.025).min(1.0) * ((LENGTH - time) / 0.025).min(1.0);
    let (start, end) = match variant % 4 {
        0 => (500.0, 1800.0),
        1 => (1900.0, 550.0),
        2 => (750.0, 2700.0),
        _ => (2800.0, 900.0),
    };
    let sweep = (end - start) / LENGTH;
    let phase = 2.0 * PI * (start * time + 0.5 * sweep * time * time);
    0.12 * fade * phase.sin()
}
fn render_value(frame: u64, rate: u32, schedule: &[(usize, u8)]) -> f32 {
    let time = frame as f32 / rate as f32;
    schedule
        .iter()
        .map(|(start_ms, variant)| probe(time - *start_ms as f32 / 1000.0, *variant))
        .sum()
}
fn downsample(input: &[f32], source_rate: u32, target_rate: usize) -> Vec<f32> {
    let length = input.len().saturating_mul(target_rate) / source_rate as usize;
    (0..length)
        .map(|i| input[i.saturating_mul(source_rate as usize) / target_rate])
        .collect()
}
fn locate_probe(capture: &[f32], start_ms: usize, variant: u8) -> Option<(i32, f32)> {
    const RATE: usize = 8000;
    let reference: Vec<f32> = (0..350 * RATE / 1000)
        .map(|i| probe(i as f32 / RATE as f32, variant))
        .collect();
    let reference_energy: f32 = reference.iter().map(|v| v * v).sum();
    let first = start_ms.saturating_sub(40) * RATE / 1000;
    let last = ((start_ms + 500) * RATE / 1000).min(capture.len().saturating_sub(reference.len()));
    let mut best = (0usize, 0.0f32);
    for offset in first..=last {
        let window = &capture[offset..offset + reference.len()];
        let energy: f32 = window.iter().map(|v| v * v).sum();
        if energy < 1e-8 {
            continue;
        }
        let dot: f32 = reference.iter().zip(window).map(|(a, b)| a * b).sum();
        let correlation = dot.abs() / (reference_energy * energy).sqrt();
        if correlation > best.1 {
            best = (offset, correlation);
        }
    }
    (best.1 > 0.0).then(|| ((best.0 * 1000 / RATE) as i32 - start_ms as i32, best.1))
}

fn calibrate_once(
    microphone_id: &str,
    speaker_id: &str,
    variants: &[u8],
) -> Result<Calibration, String> {
    for id in [microphone_id, speaker_id] {
        if id.is_empty() || id.len() > 500 || id.contains(['\r', '\n', '\0']) {
            return Err(message("AUDIO_DEVICE_ARGUMENT_INVALID"));
        }
    }
    let _com = Com::new().map_err(error)?;
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(error)?;
        let microphone_wide: Vec<u16> = microphone_id.encode_utf16().chain(Some(0)).collect();
        let speaker_wide: Vec<u16> = speaker_id.encode_utf16().chain(Some(0)).collect();
        let microphone = if microphone_id == "default" {
            enumerator.GetDefaultAudioEndpoint(eCapture, eCommunications)
        } else {
            enumerator.GetDevice(PCWSTR(microphone_wide.as_ptr()))
        }
        .map_err(error)?;
        let speaker = if speaker_id == "default" {
            enumerator.GetDefaultAudioEndpoint(eRender, eCommunications)
        } else {
            enumerator.GetDevice(PCWSTR(speaker_wide.as_ptr()))
        }
        .map_err(error)?;
        if microphone.GetState().map_err(error)?.0 & DEVICE_STATE_ACTIVE.0 == 0
            || speaker.GetState().map_err(error)?.0 & DEVICE_STATE_ACTIVE.0 == 0
        {
            return Err(message("AUDIO_DEVICE_UNAVAILABLE"));
        }
        let schedule: Vec<(usize, u8)> = variants
            .iter()
            .enumerate()
            .map(|(index, variant)| (300 + index * 750, *variant))
            .collect();
        let duration_ms = schedule.last().unwrap().0 + 900;
        let capture_client: IAudioClient = microphone.Activate(CLSCTX_ALL, None).map_err(error)?;
        let render_client: IAudioClient = speaker.Activate(CLSCTX_ALL, None).map_err(error)?;
        let capture_raw = capture_client.GetMixFormat().map_err(error)?;
        let render_raw = render_client.GetMixFormat().map_err(error)?;
        let capture_format = mix_format(capture_raw);
        let render_format = mix_format(render_raw);
        if let Err(message) = &capture_format {
            CoTaskMemFree(Some(capture_raw.cast()));
            CoTaskMemFree(Some(render_raw.cast()));
            return Err(message.clone());
        }
        if let Err(message) = &render_format {
            CoTaskMemFree(Some(capture_raw.cast()));
            CoTaskMemFree(Some(render_raw.cast()));
            return Err(message.clone());
        }
        let capture_format = capture_format?;
        let render_format = render_format?;
        let capture_init = capture_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_NOPERSIST,
            0,
            0,
            capture_raw,
            None,
        );
        let render_init = render_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_NOPERSIST,
            0,
            0,
            render_raw,
            None,
        );
        CoTaskMemFree(Some(capture_raw.cast()));
        CoTaskMemFree(Some(render_raw.cast()));
        capture_init.map_err(error)?;
        render_init.map_err(error)?;
        let capture: IAudioCaptureClient = capture_client.GetService().map_err(error)?;
        let render: IAudioRenderClient = render_client.GetService().map_err(error)?;
        let render_frames = render_client.GetBufferSize().map_err(error)?;
        let first = render.GetBuffer(render_frames).map_err(error)?;
        for frame in 0..render_frames as usize {
            write_sample(
                first,
                frame,
                render_format,
                render_value(frame as u64, render_format.rate, &schedule),
            );
        }
        render.ReleaseBuffer(render_frames, 0).map_err(error)?;
        capture_client.Start().map_err(error)?;
        render_client.Start().map_err(error)?;
        let mut rendered = render_frames as u64;
        let target_capture = capture_format.rate as usize * duration_ms / 1000;
        let deadline = Instant::now() + Duration::from_millis(duration_ms as u64 + 2500);
        let mut captured = Vec::with_capacity(target_capture + capture_format.rate as usize / 2);
        while captured.len() < target_capture && Instant::now() < deadline {
            let padding = render_client.GetCurrentPadding().map_err(error)?;
            let available = render_frames.saturating_sub(padding);
            if available > 0 {
                let data = render.GetBuffer(available).map_err(error)?;
                for frame in 0..available as usize {
                    write_sample(
                        data,
                        frame,
                        render_format,
                        render_value(rendered + frame as u64, render_format.rate, &schedule),
                    );
                }
                render.ReleaseBuffer(available, 0).map_err(error)?;
                rendered += available as u64;
            }
            loop {
                let packet = capture.GetNextPacketSize().map_err(error)?;
                if packet == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0;
                let mut flags = 0;
                capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .map_err(error)?;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                for frame in 0..frames as usize {
                    captured.push(if silent {
                        0.0
                    } else {
                        read_sample(data, frame, capture_format)
                    });
                }
                capture.ReleaseBuffer(frames).map_err(error)?;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = render_client.Stop();
        let _ = capture_client.Stop();
        if captured.len() < target_capture * 9 / 10 {
            return Err(message("CALIBRATION_NOT_ENOUGH_DATA"));
        }
        let reduced = downsample(&captured, capture_format.rate, 8000);
        let detected: Vec<Option<(i32, f32)>> = schedule
            .iter()
            .map(|(start, variant)| locate_probe(&reduced, *start, *variant))
            .collect();
        let mut estimates: Vec<(i32, f32)> = detected
            .into_iter()
            .flatten()
            .filter(|(_, correlation)| *correlation >= 0.08)
            .collect();
        let required = variants.len();
        if estimates.len() < required {
            return Err(message_with(
                "CALIBRATION_SIGNAL_UNCLEAR",
                [estimates.len(), schedule.len()],
            ));
        }
        estimates.sort_by_key(|estimate| estimate.0);
        let minimum = estimates.first().unwrap().0;
        let maximum = estimates.last().unwrap().0;
        let spread = maximum - minimum;
        if spread > 20 {
            return Err(
                message("CALIBRATION_TOO_UNSTABLE"),
            );
        }
        let measured = (estimates.iter().map(|estimate| estimate.0).sum::<i32>()
            / estimates.len() as i32)
            .clamp(0, 500) as u16;
        let mean_correlation =
            estimates.iter().map(|estimate| estimate.1).sum::<f32>() / estimates.len() as f32;
        let stability = 1.0 - (spread as f32 / 40.0).min(0.35);
        let confidence = (mean_correlation * stability * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8;
        Ok(Calibration {
            measured_ms: measured,
            recommended_ms: measured,
            confidence,
            samples: estimates.len() as u8,
            spread_ms: spread as u16,
            stable: true,
        })
    }
}

fn aggregate_careful(
    mut runs: Vec<Calibration>,
    attempted_sessions: usize,
    last_error: &str,
) -> Result<Calibration, String> {
    if runs.is_empty() {
        return Err(message_with("CALIBRATION_NO_RESULT", [last_error]));
    }
    let enough_sessions = runs.len() >= 4;
    let successful_sessions = runs.len();
    runs.sort_by_key(|run| run.measured_ms);
    let center = runs[runs.len() / 2].measured_ms as i32;
    let mut deviations: Vec<i32> = runs
        .iter()
        .map(|run| (run.measured_ms as i32 - center).abs())
        .collect();
    deviations.sort_unstable();
    let tolerance = (deviations[deviations.len() / 2] * 3).clamp(6, 18);
    runs.retain(|run| (run.measured_ms as i32 - center).abs() <= tolerance);
    let enough_consensus = runs.len() >= 3;
    runs.sort_by_key(|run| run.measured_ms);
    let minimum = runs.first().unwrap().measured_ms;
    let maximum = runs.last().unwrap().measured_ms;
    let spread = maximum - minimum;
    let stable = enough_sessions && enough_consensus && spread <= 10;
    let measured = if runs.len() % 2 == 0 {
        let high = runs.len() / 2;
        (runs[high - 1].measured_ms + runs[high].measured_ms) / 2
    } else {
        runs[runs.len() / 2].measured_ms
    };
    let mean_confidence =
        runs.iter().map(|run| run.confidence as u32).sum::<u32>() / runs.len() as u32;
    let stability = (1.0 - (spread as f32 / 40.0).min(0.50))
        * (successful_sessions as f32 / attempted_sessions as f32);
    Ok(Calibration {
        measured_ms: measured,
        recommended_ms: measured,
        confidence: (mean_confidence as f32 * stability).round() as u8,
        samples: runs.iter().map(|run| run.samples).sum(),
        spread_ms: spread,
        stable,
    })
}

pub fn calibrate_aec(
    microphone_id: &str,
    speaker_id: &str,
    careful: bool,
) -> Result<Calibration, String> {
    if !careful {
        return calibrate_once(microphone_id, speaker_id, &[0, 0]);
    }
    // Independent WASAPI sessions expose startup/buffering jitter which a
    // single long stream cannot detect. Vary the excitation spectrum too.
    let patterns = [[0, 1], [2, 3], [1, 2], [3, 0], [0, 2]];
    let mut runs = Vec::with_capacity(patterns.len());
    let mut last_error = String::new();
    for variants in patterns {
        match calibrate_once(microphone_id, speaker_id, &variants) {
            Ok(run) => runs.push(run),
            Err(message) => last_error = message,
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    aggregate_careful(runs, patterns.len(), &last_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_finds_known_delay() {
        let mut capture = vec![0.0; 32_000];
        let delay_ms = 73;
        for (variant, start_ms) in [300, 1050, 1800, 2550].into_iter().enumerate() {
            for i in 0..2800 {
                let target = (start_ms + delay_ms) * 8 + i;
                capture[target] += probe(i as f32 / 8000.0, variant as u8);
            }
            let found = locate_probe(&capture, start_ms, variant as u8).unwrap();
            assert!((found.0 - delay_ms as i32).abs() <= 1);
            assert!(found.1 > 0.99);
        }
    }

    #[test]
    fn unstable_careful_measurement_still_returns_a_reference_value() {
        let run = |measured_ms| Calibration {
            measured_ms,
            recommended_ms: measured_ms,
            confidence: 80,
            samples: 2,
            spread_ms: 0,
            stable: true,
        };
        let result =
            aggregate_careful(vec![run(300), run(340), run(380), run(420)], 5, "").unwrap();
        assert!(!result.stable);
        assert_eq!(result.recommended_ms, 380);
        assert_eq!(result.samples, 2);
    }

    #[test]
    #[ignore = "plays two audible probes through the configured Windows communication devices"]
    fn real_default_device_calibration() {
        let microphone = std::env::var("KSIP_TEST_MICROPHONE").unwrap_or_else(|_| "default".into());
        let speaker = std::env::var("KSIP_TEST_SPEAKER").unwrap_or_else(|_| "default".into());
        let result = calibrate_aec(&microphone, &speaker, false).unwrap();
        eprintln!(
            "measured={}ms confidence={}%",
            result.measured_ms, result.confidence
        );
    }
}
