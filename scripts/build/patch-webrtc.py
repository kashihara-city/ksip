"""Install the tracked KSIP bridge and an idempotent GN target in WebRTC."""
# What is changed in the pinned WebRTC checkout, and why. Every change is
# marked in the source and re-applied over its own marks, so running this
# twice gives the same result.
#
# Behaviour of the audio device (the only change of that kind):
#   modules/audio_device/win/core_audio_utility_win.{cc,h}
#   modules/audio_device/win/core_audio_base_win.cc
#       the WASAPI capture stream asks for AUDCLNT_STREAMOPTIONS_RAW, so the
#       microphone is read before Windows' or the OEM's communication APOs
#       (their echo cancellation, noise suppression and gain) touch it. KSIP
#       runs Google's APM itself, and the two in series once cut the input to
#       around -90 dBFS. If the device refuses RAW, the stream is opened again
#       without it. Playback is not changed.
#
# Building and embedding (no change to what the library does):
#   ksip_bridge/                   the KSIP bridge sources copied from
#                                  src-native/webrtc.
#   BUILD.gn                       a static library target ksip_webrtc_audio
#                                  that bundles the bridge with the ADM, APM
#                                  and their dependencies.
#
# Nothing else in WebRTC is touched.
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
TARGET = f'''{BEGIN}
rtc_static_library("ksip_webrtc_audio") {{
  visibility = [ "*" ]
  allow_poison = [ "environment_construction" ]
  sources = [ "ksip_bridge/ksip_audio_bridge.cc" ]
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
  // KSIP runs Google APM itself. Avoid applying an endpoint communication
  // APO (AEC/NS/AGC) to the same capture stream first.
  if (raw_capture) props.Options |= AUDCLNT_STREAMOPTIONS_RAW;
  {RAW_END}"""
    set_properties_replacement = f"""  error = client->SetClientProperties(&props);
  {RAW_FALLBACK_BEGIN}
#if (NTDDI_VERSION >= NTDDI_WINBLUE)
  if (FAILED(error.Error()) && raw_capture &&
      props.Options == AUDCLNT_STREAMOPTIONS_RAW) {{
    RTC_LOG(LS_WARNING)
        << "RAW audio capture is unavailable; retrying without RAW mode";
    props.Options = AUDCLNT_STREAMOPTIONS_NONE;
    error = client->SetClientProperties(&props);
  }}
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

    old_signature = "HRESULT SetClientProperties(IAudioClient2* client) {"
    new_signature = ("HRESULT SetClientProperties(IAudioClient2* client, "
                     "bool raw_capture) {")
    if new_signature not in text:
        if old_signature not in text:
            raise RuntimeError(
                "Unsupported WebRTC CoreAudio source: function anchor missing")
        text = text.replace(old_signature, new_signature, 1)
    core_audio.write_text(text, encoding="utf-8")

    header = (source / "modules" / "audio_device" / "win" /
              "core_audio_utility_win.h")
    header_text = header.read_text(encoding="utf-8")
    old_declaration = "HRESULT SetClientProperties(IAudioClient2* client);"
    new_declaration = ("HRESULT SetClientProperties(IAudioClient2* client, "
                       "bool raw_capture = false);")
    if new_declaration not in header_text:
        if old_declaration not in header_text:
            raise RuntimeError(
                "Unsupported WebRTC CoreAudio header: declaration anchor missing")
        header_text = header_text.replace(old_declaration, new_declaration, 1)
        header.write_text(header_text, encoding="utf-8")

    base = (source / "modules" / "audio_device" / "win" /
            "core_audio_base_win.cc")
    base_text = base.read_text(encoding="utf-8")
    old_call = """core_audio_utility::SetClientProperties(
            static_cast<IAudioClient2*>(audio_client.Get()))"""
    new_call = """core_audio_utility::SetClientProperties(
            static_cast<IAudioClient2*>(audio_client.Get()),
            GetDataFlow() == eCapture)"""
    if new_call not in base_text:
        if old_call not in base_text:
            raise RuntimeError(
                "Unsupported WebRTC CoreAudio base: call anchor missing")
        base_text = base_text.replace(old_call, new_call, 1)
        base.write_text(base_text, encoding="utf-8")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    source = args.source.resolve()
    patch_raw_capture(source)
    bridge = source / "ksip_bridge"
    bridge.mkdir(exist_ok=True)
    for name in ["ksip_audio_bridge.h", "ksip_audio_bridge_internal.h",
                 "ksip_audio_bridge.cc"]:
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
