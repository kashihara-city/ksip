"""Install the tracked KSIP bridge and an idempotent GN target in WebRTC."""
# What is changed in the pinned WebRTC checkout, and why. Every change is
# marked in the source and re-applied over its own marks, so running this
# twice gives the same result.
#
# Behaviour of the audio device:
#   modules/audio_device/win/core_audio_utility_win.{cc,h}
#   modules/audio_device/win/core_audio_base_win.cc
#       the WASAPI streams ask for AUDCLNT_STREAMOPTIONS_RAW when the bridge
#       wants it (KsipWantRaw; both, by default), so that no endpoint APO
#       changes the audio KSIP processes. The microphone is read before
#       Windows' or the OEM's communication APOs (their echo cancellation,
#       noise suppression and gain) touch it: KSIP runs Google's APM itself,
#       and the two in series once cut the input to around -90 dBFS. The
#       speaker plays what KSIP sent, which the echo cancellation takes as its
#       reference, without bass, surround or loudness effects. If the device
#       refuses RAW, the stream is opened again without it; which of the two a
#       stream got is told to the bridge (KsipNoteRaw), which shows it. The
#       endpoint a stream opens is told too (KsipNoteDevice), the default
#       WebRTC moves to by itself when a device goes away included, so that
#       the microphone's mute is read off the one in use.
#   modules/audio_device/win/core_audio_{input,output}_win.cc
#       StartRecording and StartPlayout mark the stream active before Start()
#       spawns the audio thread, not after it returns. The thread's first
#       data callback could run in between, find the stream inactive, and end
#       the thread for good: the device stayed open and the start reported
#       success, but no audio was delivered again. A call then sent no RTP,
#       heard nothing from a PBX that waits for it, and recorded nothing.
#   modules/audio_device/win/core_audio_input_win.cc
#       a capture packet Windows marks AUDCLNT_BUFFERFLAGS_SILENT is delivered
#       as the zeros WebRTC already fills it with, instead of being dropped.
#       The flag means the data is to be taken as silence, not that there is
#       none; dropped, a call stops sending RTP for as long as it lasts (a
#       device may mark its packets so while muted), with what that brings: a
#       PBX that waits for RTP sends none back, and a firewall closes the way
#       in. The three microphones tried did not mark muted packets so.
#
# Building and embedding (no change to what the library does):
#   ksip_bridge/                   the KSIP bridge sources copied from
#                                  src-native/webrtc.
#   BUILD.gn                       a static library target ksip_webrtc_audio
#                                  that bundles the bridge with the ADM, APM
#                                  and their dependencies.
#
# Nothing else in WebRTC is touched (scripts/test/supply-chain.py holds the
# same list).
from pathlib import Path
import argparse
import shutil

ROOT = Path(__file__).resolve().parents[2]
BEGIN = "# KSIP AUDIO BRIDGE BEGIN"
END = "# KSIP AUDIO BRIDGE END"
RAW_BEGIN = "// KSIP RAW CAPTURE BEGIN"
RAW_END = "// KSIP RAW CAPTURE END"
RAW_FALLBACK_BEGIN = "// KSIP RAW FALLBACK BEGIN"
RAW_FALLBACK_END = "// KSIP RAW FALLBACK END"
DEVICE_DECLARATION = ("// KSIP: defined by the bridge: the endpoint a stream "
                      "opens, and that it opened (the RAW mode noted for it "
                      "then stands).\nvoid KsipNoteDevice(bool capture, "
                      "const std::string& id);\nvoid KsipNoteOpened(bool capture);")
DEVICE_ANCHOR = """  RTC_LOG(LS_INFO) << "Unique device identifier: device_id=" << device_id
                   << ", role=" << RoleToString(role);
"""
DEVICE_CALL = """  // KSIP: the endpoint this stream opens, for the bridge.
  core_audio_utility::KsipNoteDevice(GetDataFlow() == eCapture, device_id_);
"""
OPENED_ANCHOR = """  audio_client_ = audio_client;
  audio_session_control_ = audio_session_control;

  return true;
}"""
OPENED_CALL = """  audio_client_ = audio_client;
  audio_session_control_ = audio_session_control;

  // KSIP: opened; the RAW mode noted for it stands.
  core_audio_utility::KsipNoteOpened(GetDataFlow() == eCapture);
  return true;
}"""
ACTIVE_BEGIN = "// KSIP ACTIVE BEFORE START BEGIN"
ACTIVE_END = "// KSIP ACTIVE BEFORE START END"
SILENT_BEGIN = "// KSIP SILENT DELIVERED BEGIN"
SILENT_END = "// KSIP SILENT DELIVERED END"
TARGET = f'''{BEGIN}
rtc_static_library("ksip_webrtc_audio") {{
  visibility = [ "*" ]
  allow_poison = [ "environment_construction" ]
  sources = [
    "ksip_bridge/apm.cc",
    "ksip_bridge/device_selection.cc",
    "ksip_bridge/ksip_audio_bridge.cc",
    "ksip_bridge/pcm_processing.cc",
  ]
  complete_static_lib = true
  suppressed_configs += [ "//build/config/compiler:thin_archive" ]
  deps = [
    "api/audio:builtin_audio_processing_builder",
    "api/environment:environment_factory",
    "common_audio",
    "modules/audio_device:audio_device_module_from_input_and_output",
    "modules/audio_processing",
    "rtc_base/win:scoped_com_initializer",
  ]
}}
{END}
'''


def replace_one_of(text: str, forms: list[str], new: str, what: str) -> str:
    """The patched form, put over WebRTC's own or an earlier KSIP one."""
    if new in text:
        return text
    for form in forms:
        if form in text:
            return text.replace(form, new, 1)
    raise RuntimeError(f"Unsupported WebRTC CoreAudio source: {what} anchor missing")


def patch_raw_capture(source: Path):
    """Bypass Windows communication APOs before WebRTC APM when supported."""
    core_audio = (source / "modules" / "audio_device" / "win" /
                  "core_audio_utility_win.cc")
    text = core_audio.read_text(encoding="utf-8")
    markers = [RAW_BEGIN, RAW_END, RAW_FALLBACK_BEGIN, RAW_FALLBACK_END]
    present = [marker in text for marker in markers]
    if any(present) and not all(present):
        raise RuntimeError(
            "Unsupported partial KSIP RAW patch in core_audio_utility_win.cc")
    raw_replacement = f"""  {RAW_BEGIN}
  // KSIP runs Google APM itself. Its streams take RAW mode when the bridge
  // wants it, so that no endpoint APO changes the audio around it.
  const bool raw = ksip_flow != eAll && KsipWantRaw(ksip_flow == eCapture);
  if (raw) props.Options |= AUDCLNT_STREAMOPTIONS_RAW;
  {RAW_END}"""
    set_properties_replacement = f"""  error = client->SetClientProperties(&props);
  {RAW_FALLBACK_BEGIN}
#if (NTDDI_VERSION >= NTDDI_WINBLUE)
  if (FAILED(error.Error()) && raw &&
      props.Options == AUDCLNT_STREAMOPTIONS_RAW) {{
    RTC_LOG(LS_WARNING) << "RAW audio "
                        << (ksip_flow == eCapture ? "capture" : "playout")
                        << " is unavailable; retrying without RAW mode";
    props.Options = AUDCLNT_STREAMOPTIONS_NONE;
    error = client->SetClientProperties(&props);
  }}
  if (ksip_flow != eAll && SUCCEEDED(error.Error()))
    KsipNoteRaw(ksip_flow == eCapture,
                props.Options == AUDCLNT_STREAMOPTIONS_RAW);
#endif
  {RAW_FALLBACK_END}
  if (FAILED(error.Error())) {{
    RTC_LOG(LS_ERROR) << "IAudioClient2::SetClientProperties failed: "
                      << ErrorToString(error);
  }}
  return error.Error();"""
    if not any(present):
        commented_raw = "  // props.Options |= AUDCLNT_STREAMOPTIONS_RAW;"
        enabled_raw = "  props.Options |= AUDCLNT_STREAMOPTIONS_RAW;"
        if commented_raw in text:
            raw_line = commented_raw
        elif enabled_raw in text:
            # Accept a developer A/B-test tree and make it reproducible.
            raw_line = enabled_raw
        else:
            raise RuntimeError(
                "Unsupported WebRTC CoreAudio source: RAW option anchor missing")
        text = text.replace(raw_line, raw_replacement, 1)
        set_properties = """  error = client->SetClientProperties(&props);
  if (FAILED(error.Error())) {
    RTC_LOG(LS_ERROR) << "IAudioClient2::SetClientProperties failed: "
                      << ErrorToString(error);
  }
  return error.Error();"""
        if set_properties not in text:
            raise RuntimeError(
                "Unsupported WebRTC CoreAudio source: client properties anchor missing")
        text = text.replace(set_properties, set_properties_replacement, 1)
    else:
        raw_start = text.index(RAW_BEGIN) - 2
        raw_finish = text.index(RAW_END, raw_start) + len(RAW_END)
        text = text[:raw_start] + raw_replacement + text[raw_finish:]
        fallback_start = text.index(RAW_FALLBACK_BEGIN) - 2
        fallback_finish = (text.index(RAW_FALLBACK_END, fallback_start) +
                           len(RAW_FALLBACK_END))
        new_fallback = set_properties_replacement[
            set_properties_replacement.index(RAW_FALLBACK_BEGIN) - 2:
            set_properties_replacement.index(RAW_FALLBACK_END) +
            len(RAW_FALLBACK_END)]
        text = text[:fallback_start] + new_fallback + text[fallback_finish:]

    new_signature = ("HRESULT SetClientProperties(IAudioClient2* client, "
                     "EDataFlow ksip_flow) {")
    text = replace_one_of(text, [
        "HRESULT SetClientProperties(IAudioClient2* client) {",
        "HRESULT SetClientProperties(IAudioClient2* client, bool raw_capture) {",
    ], new_signature, "function")
    core_audio.write_text(text, encoding="utf-8")

    header = (source / "modules" / "audio_device" / "win" /
              "core_audio_utility_win.h")
    header_text = header.read_text(encoding="utf-8")
    # A stream of KSIP's names its flow; eAll, the default, is one of
    # WebRTC's own, which takes no RAW mode and is not told to the bridge.
    new_declaration = ("HRESULT SetClientProperties(IAudioClient2* client, "
                       "EDataFlow ksip_flow = eAll);\n"
                       "// KSIP: defined by the bridge: whether a stream is to "
                       "take RAW mode,\n// and whether it did.\n"
                       "bool KsipWantRaw(bool capture);\n"
                       "void KsipNoteRaw(bool capture, bool raw);")
    header_text = header_text.replace(
        "\n// KSIP: defined by the bridge, told whether capture got RAW."
        "\nvoid KsipNoteCaptureRaw(bool raw);", "")
    header_text = replace_one_of(header_text, [
        "HRESULT SetClientProperties(IAudioClient2* client);",
        "HRESULT SetClientProperties(IAudioClient2* client, "
        "bool raw_capture = false);",
    ], new_declaration, "declaration")
    header_text = header_text.replace(
        "\n// KSIP: defined by the bridge: the endpoint a stream opens."
        "\nvoid KsipNoteDevice(bool capture, const std::string& id);", "")
    if DEVICE_DECLARATION not in header_text:
        header_text = header_text.replace(
            new_declaration, new_declaration + "\n" + DEVICE_DECLARATION, 1)
    header.write_text(header_text, encoding="utf-8")

    base = (source / "modules" / "audio_device" / "win" /
            "core_audio_base_win.cc")
    base_text = base.read_text(encoding="utf-8")
    call = """core_audio_utility::SetClientProperties(
            static_cast<IAudioClient2*>(audio_client.Get())"""
    base_text = replace_one_of(base_text, [
        call + ")",
        call + ",\n            GetDataFlow() == eCapture)",
    ], call + ",\n            GetDataFlow())", "call")
    # The endpoint a stream opens, whichever way it was chosen: one KSIP
    # selected, or the default WebRTC moves to on its own when the device in
    # use goes away (SwitchDeviceIfNeeded, which KSIP does not see).
    if DEVICE_CALL not in base_text:
        if DEVICE_ANCHOR not in base_text:
            raise RuntimeError(
                "Unsupported WebRTC CoreAudio base: device anchor missing")
        base_text = base_text.replace(DEVICE_ANCHOR, DEVICE_ANCHOR + DEVICE_CALL, 1)
    # A stream whose properties took RAW mode may still fail to open (a
    # microphone Windows' privacy settings refuse fails at Initialize): what
    # was noted counts once it has opened.
    if OPENED_CALL not in base_text:
        if base_text.count(OPENED_ANCHOR) != 1:
            raise RuntimeError(
                "Unsupported WebRTC CoreAudio base: opened anchor missing")
        base_text = base_text.replace(OPENED_ANCHOR, OPENED_CALL, 1)
    base.write_text(base_text, encoding="utf-8")


def patch_active_before_start(source: Path):
    """Let the audio thread's first callback find its stream active."""
    original = """  if (!Start()) {
    return -1;
  }

  is_active_ = true;
  return 0;"""
    replacement = f"""  {ACTIVE_BEGIN}
  // Active before Start() spawns the audio thread: its first data callback
  // can run before Start() returns, and one that finds the stream inactive
  // ends the thread for good while the start still reports success.
  is_active_ = true;
  if (!Start()) {{
    is_active_ = false;
    return -1;
  }}
  {ACTIVE_END}
  return 0;"""
    for name in ["core_audio_input_win.cc", "core_audio_output_win.cc"]:
        path = source / "modules" / "audio_device" / "win" / name
        text = path.read_text(encoding="utf-8")
        present = [ACTIVE_BEGIN in text, ACTIVE_END in text]
        if any(present) and not all(present):
            raise RuntimeError(f"Unsupported partial KSIP active patch in {name}")
        if all(present):
            start = text.index(ACTIVE_BEGIN) - 2
            finish = text.index(ACTIVE_END, start) + len(ACTIVE_END)
            text = text[:start] + replacement[:replacement.index(ACTIVE_END) +
                                              len(ACTIVE_END)] + text[finish:]
        else:
            if text.count(original) != 1:
                raise RuntimeError(
                    f"Unsupported WebRTC CoreAudio source: start anchor missing in {name}")
            text = text.replace(original, replacement, 1)
        path.write_text(text, encoding="utf-8")


def patch_silent_delivered(source: Path):
    """Deliver a capture packet marked silent as zeros instead of dropping it."""
    original = """    if (flags & AUDCLNT_BUFFERFLAGS_SILENT) {
      webrtc::ExplicitZeroMemory(
          audio_data, format_.Format.nBlockAlign * num_frames_to_read);
      RTC_DLOG(LS_WARNING) << "Captured audio is replaced by silence";
    } else {
      // Copy recorded audio in `audio_data` to the WebRTC sink using the
      // FineAudioBuffer object.
      fine_audio_buffer_->DeliverRecordedData(
          std::span(reinterpret_cast<const int16_t*>(audio_data),
                    format_.Format.nChannels * num_frames_to_read),

          latency_ms_);
    }"""
    replacement = f"""    {SILENT_BEGIN}
    // Silent means the data is to be taken as silence: it goes on as the
    // zeros filled in here, so that the stream does not stop while it lasts.
    if (flags & AUDCLNT_BUFFERFLAGS_SILENT) {{
      webrtc::ExplicitZeroMemory(
          audio_data, format_.Format.nBlockAlign * num_frames_to_read);
      RTC_DLOG(LS_WARNING) << "Captured audio is replaced by silence";
    }}
    // Copy recorded audio in `audio_data` to the WebRTC sink using the
    // FineAudioBuffer object.
    fine_audio_buffer_->DeliverRecordedData(
        std::span(reinterpret_cast<const int16_t*>(audio_data),
                  format_.Format.nChannels * num_frames_to_read),

        latency_ms_);
    {SILENT_END}"""
    path = source / "modules" / "audio_device" / "win" / "core_audio_input_win.cc"
    text = path.read_text(encoding="utf-8")
    present = [SILENT_BEGIN in text, SILENT_END in text]
    if any(present) and not all(present):
        raise RuntimeError("Unsupported partial KSIP silent patch in core_audio_input_win.cc")
    if all(present):
        start = text.index(SILENT_BEGIN) - 4
        finish = text.index(SILENT_END, start) + len(SILENT_END)
        text = text[:start] + replacement + text[finish:]
    else:
        if text.count(original) != 1:
            raise RuntimeError(
                "Unsupported WebRTC CoreAudio source: silent packet anchor missing")
        text = text.replace(original, replacement, 1)
    path.write_text(text, encoding="utf-8")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    source = args.source.resolve()
    patch_raw_capture(source)
    patch_active_before_start(source)
    patch_silent_delivered(source)
    bridge = source / "ksip_bridge"
    bridge.mkdir(exist_ok=True)
    for name in ["ksip_audio_bridge.h", "ksip_audio_bridge_internal.h",
                 "bridge_state.h", "callback_gate.h",
                 "ksip_audio_bridge.cc", "apm.cc", "device_selection.cc",
                 "pcm_processing.cc"]:
        shutil.copy2(ROOT / "src-native/webrtc" / name, bridge / name)
    build = source / "BUILD.gn"
    text = build.read_text(encoding="utf-8")
    if BEGIN in text:
        start = text.index(BEGIN)
        finish = text.index(END, start) + len(END)
        text = text[:start] + text[finish:].lstrip("\r\n")
    anchor = "# ---- Poisons ----"
    if anchor not in text:
        raise RuntimeError("Unsupported WebRTC BUILD.gn: poison anchor missing")
    build.write_text(text.replace(anchor, TARGET + "\n" + anchor, 1),
                     encoding="utf-8")


if __name__ == "__main__":
    main()
