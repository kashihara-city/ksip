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
#       the microphone's mute is read off the one in use, and that it opened
#       (KsipNoteOpened), so that what is told is what is open.
#   modules/audio_device/win/core_audio_base_win.cc
#       the default WebRTC moves a stream to by itself when the device in use
#       goes away (SwitchDeviceIfNeeded) is the default communications
#       device, not the default (console) one: the one KSIP's own rule, the
#       alert sounds and the app's window use in place of a device that is
#       not there, so that a call whose device is unplugged goes on where the
#       rest of KSIP would put it. On a machine with one default for both
#       roles there is no difference.
#   modules/audio_device/win/core_audio_base_win.cc
#       a restart WebRTC attempts by itself that fails (the device in use
#       went away and there is no other to move to: the only microphone
#       unplugged) left the audio thread ending on a client the restart had
#       already released, which crashed the engine, and left that ended
#       thread unjoined, so that the next start spawned no live thread and
#       delivered nothing. The thread's exit stops no client it no longer
#       has, and a start joins a thread left over before spawning one.
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
# What the echo canceller says (no change to what it does):
#   modules/audio_processing/aec3/echo_remover.cc
#   modules/audio_processing/aec3/subtractor.{cc,h}
#       AEC3 tells nothing of how it is cancelling. Each block (4 ms) its own
#       judgments are told to the bridge (KsipNoteAec3), which hands them to
#       the app: whether it took the device for one without echo (transparent
#       mode, where it all but stops suppressing), whether its linear filter
#       is used, whether the filter converged (the analysis AecState makes of
#       the subtractor's output, run once more on the same output), whether
#       the refined filter adapted (its update gain was not all zero) or was
#       scaled back as gone wrong (the subtractor keeps both), and whether the
#       echo is clipped.
#       Each change of the transparent mode, the linear filter's use and the
#       clipping is logged as a warning, with the seconds since the canceller
#       started, so that it shows in a log without the detail log; and every
#       10 s a line (at the info level, the detail log's) says how the last
#       10 s went: the share of blocks with the far end active (above AEC3's
#       own threshold), in transparent mode, with the linear filter used, with
#       the filter converged and adapting, with both filters over the
#       microphone (the analysis's "diverged", which AEC3 itself does not act
#       on block by block), and with the near end dominant, the times the
#       filter was scaled back, and the canceller's ERLE, ERL and filter delay.
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
import re
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
SWITCH_ORIGINAL = "    if (SetDevice(kDefault) == -1) {"
SWITCH_PATCHED = ("    // KSIP: the default communications device, as KSIP opens in place of a"
                  " device that is not there.\n"
                  "    if (SetDevice(kDefaultCommunications) == -1) {")
THREAD_EXIT_ORIGINAL = "    result = audio_client_->Stop();\n"
THREAD_EXIT_PATCHED = ("    // KSIP: no client to stop when a restart that failed has released it.\n"
                       "    if (audio_client_) result = audio_client_->Stop();\n")
THREAD_SPAWN_ORIGINAL = """  if (audio_thread_.empty()) {
    const absl::string_view name =
        IsInput() ? "wasapi_capture_thread" : "wasapi_render_thread";"""
THREAD_SPAWN_PATCHED = """  // KSIP: a thread left by a restart that failed (its loop ended, nothing
  // joined it) is joined first, so that a live one is spawned.
  if (!IsRestarting() && !audio_thread_.empty()) StopThread();
""" + THREAD_SPAWN_ORIGINAL
START_FAIL_ORIGINAL = """  if (FAILED(error.Error())) {
    StopThread();
    RTC_LOG(LS_ERROR) << "IAudioClient::Start failed: \""""
START_FAIL_PATCHED = """  if (FAILED(error.Error())) {
    // KSIP: not from the audio thread itself, which runs the internal
    // restart: it would wait for its own end. That thread ends by itself
    // once the restart handler returns false, and the next stop or start
    // joins it.
    if (!IsRestarting()) StopThread();
    RTC_LOG(LS_ERROR) << "IAudioClient::Start failed: \""""
ACTIVE_BEGIN = "// KSIP ACTIVE BEFORE START BEGIN"
ACTIVE_END = "// KSIP ACTIVE BEFORE START END"
SILENT_BEGIN = "// KSIP SILENT DELIVERED BEGIN"
SILENT_END = "// KSIP SILENT DELIVERED END"
AEC3_BEGIN = "// KSIP AEC3 STATE BEGIN"
AEC3_END = "// KSIP AEC3 STATE END"
# Each insertion of the AEC3 state patch: the file (in aec3/), the line it
# goes by, whether it goes before or after that line, and what goes between
# the marks.
AEC3_INSERTIONS = [
    ("echo_remover.cc", '#include "rtc_base/logging.h"\n', "after", """\
#include "modules/audio_processing/aec3/subtractor_output_analyzer.h"

namespace webrtc {
// Defined by the KSIP bridge: what AEC3 judged of each block (4 ms).
void KsipNoteAec3(bool transparent,
                  bool linear,
                  bool converged,
                  bool learning,
                  bool saturated_echo,
                  bool diverged);
}  // namespace webrtc
"""),
    ("echo_remover.cc", "  bool ml_ree_was_active_ = false;\n", "after", """\
  bool ksip_transparent_ = false;
  bool ksip_linear_ = false;
  bool ksip_saturated_ = false;
  // AEC3's own analysis of the subtractor's output, as AecState makes it.
  SubtractorOutputAnalyzer ksip_analyzer_{num_capture_channels_};
  size_t ksip_window_ = 0;
  size_t ksip_render_ = 0;
  size_t ksip_transparent_blocks_ = 0;
  size_t ksip_linear_blocks_ = 0;
  size_t ksip_converged_blocks_ = 0;
  size_t ksip_learning_blocks_ = 0;
  size_t ksip_diverged_blocks_ = 0;
  size_t ksip_rescales_ = 0;
  size_t ksip_nearend_blocks_ = 0;
"""),
    ("echo_remover.cc",
     "  metrics_.Update(aec_state_, cng_.NoiseSpectrum()[0], G);\n", "after", """\
  {
    // Seconds since this canceller started, at one decimal.
    const double seconds =
        static_cast<int>(block_counter_ * 10 / kNumBlocksPerSecond) / 10.0;
    const bool transparent = aec_state_.TransparentModeActive();
    const bool linear = aec_state_.UsableLinearEstimate();
    const bool saturated = aec_state_.SaturatedEcho();
    // The block's filters as AEC3 judges them: converged by the analysis
    // AecState makes of the subtractor's output, adapted, and rescaled. A
    // block whose filters both came out over the microphone (the analysis's
    // "diverged") happens now and then and is only counted: AEC3 acts on it
    // only when 60 come in a row, and only in the legacy transparent mode.
    // What it does about a filter gone wrong is the rescale.
    bool converged = false;
    bool coarse_converged = false;
    bool diverged = false;
    ksip_analyzer_.Update(subtractor_output, &converged, &coarse_converged,
                          &diverged);
    const bool learning = subtractor_.KsipRefinedAdapted();
    const bool rescaled = subtractor_.KsipRefinedRescaled();
    KsipNoteAec3(transparent, linear, converged, learning, saturated,
                 rescaled);
    if (transparent != ksip_transparent_ || linear != ksip_linear_ ||
        saturated != ksip_saturated_) {
      RTC_LOG(LS_WARNING) << "AEC3 state at " << seconds
                          << " s: transparent " << (transparent ? "on" : "off")
                          << ", linear filter "
                          << (linear ? "used" : "not used")
                          << ", saturated echo " << (saturated ? "yes" : "no");
      ksip_transparent_ = transparent;
      ksip_linear_ = linear;
      ksip_saturated_ = saturated;
    }
    // The far end active as AEC3 counts it (aec_state.cc): the block's
    // energy above active_render_limit squared per sample.
    float render_energy = 0.f;
    for (float v : x.View(/*band=*/0, /*channel=*/0)) render_energy += v * v;
    constexpr float kActiveRender = 100.f * 100.f * kFftLengthBy2;
    ++ksip_window_;
    ksip_render_ += render_energy > kActiveRender ? 1 : 0;
    ksip_transparent_blocks_ += transparent ? 1 : 0;
    ksip_linear_blocks_ += linear ? 1 : 0;
    ksip_converged_blocks_ += converged ? 1 : 0;
    ksip_learning_blocks_ += learning ? 1 : 0;
    ksip_diverged_blocks_ += diverged ? 1 : 0;
    ksip_rescales_ += rescaled ? 1 : 0;
    ksip_nearend_blocks_ += suppression_gain_.IsDominantNearend() ? 1 : 0;
    if (ksip_window_ == 10 * kNumBlocksPerSecond) {
      const auto share = [this](size_t n) {
        return static_cast<int>(100 * n / ksip_window_);
      };
      RTC_LOG(LS_INFO) << "AEC3 last 10 s at " << seconds
                       << " s: far end active " << share(ksip_render_)
                       << "%, transparent " << share(ksip_transparent_blocks_)
                       << "%, linear filter " << share(ksip_linear_blocks_)
                       << "%, filter converged "
                       << share(ksip_converged_blocks_) << "%, learning "
                       << share(ksip_learning_blocks_)
                       << "%, both filters over the microphone "
                       << share(ksip_diverged_blocks_)
                       << "%, filter rescaled " << ksip_rescales_
                       << " times, near end dominant "
                       << share(ksip_nearend_blocks_) << "%, ERLE "
                       << static_cast<int>(3.0103f *
                                           aec_state_.FullBandErleLog2())
                       << " dB, ERL "
                       << static_cast<int>(
                              10.f * std::log10(std::max(
                                         aec_state_.ErlTimeDomain(), 1e-6f)))
                       << " dB, filter delay "
                       << aec_state_.MinDirectPathFilterDelay() << " blocks";
      ksip_window_ = ksip_render_ = ksip_transparent_blocks_ =
          ksip_linear_blocks_ = ksip_converged_blocks_ =
              ksip_learning_blocks_ = ksip_diverged_blocks_ = ksip_rescales_ =
                  ksip_nearend_blocks_ = 0;
    }
  }
"""),
    ("subtractor.h", "  void DumpFilters() {\n", "before", """\
  // Whether the refined filter adapted in the last block: its update gain
  // was not all zero. It is while the render excites the filter and the
  // capture is not saturated, and not just after a reset.
  bool KsipRefinedAdapted() const { return ksip_refined_adapted_; }
  // Whether the refined filter was scaled back in the last block: its error
  // had come out loud and far over the microphone (the misadjustment
  // estimator), which is how AEC3 finds a filter gone wrong.
  bool KsipRefinedRescaled() const { return ksip_refined_rescaled_; }
"""),
    ("subtractor.h",
     "  std::vector<std::vector<float>> coarse_impulse_responses_;\n", "after",
     """\
  bool ksip_refined_adapted_ = false;
  bool ksip_refined_rescaled_ = false;
"""),
    ("subtractor.cc",
     "    refined_filters_[ch]->Adapt(render_buffer, G,\n"
     "                                &refined_impulse_responses_[ch]);\n",
     "before", """\
    if (ch == 0) {
      ksip_refined_adapted_ = false;
      ksip_refined_rescaled_ = false;
    }
    ksip_refined_rescaled_ = ksip_refined_rescaled_ || refined_filters_adjusted;
    for (size_t k = 0; k < kFftLengthBy2Plus1; ++k) {
      ksip_refined_adapted_ =
          ksip_refined_adapted_ || G.re[k] != 0.f || G.im[k] != 0.f;
    }
"""),
]
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
    "api:field_trials",
    "api/audio:aec3_config",
    "api/audio:builtin_audio_processing_builder",
    "api/environment:environment_factory",
    "common_audio",
    "modules/audio_device:audio_device_module_from_input_and_output",
    "modules/audio_device:windows_core_audio_utility",
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
    # The device WebRTC moves a stream to by itself when the one in use goes
    # away: the default communications device, KSIP's own stand-in.
    if base_text.count(SWITCH_ORIGINAL) + base_text.count(SWITCH_PATCHED) != 1:
        raise RuntimeError(
            "Unsupported WebRTC CoreAudio base: device switch anchor missing")
    base_text = replace_one_of(base_text, [SWITCH_ORIGINAL], SWITCH_PATCHED, "device switch")
    # A restart that failed: the audio thread's exit stops no client the
    # restart released, the next start joins the thread it left, and a
    # start that fails on that thread does not wait for the thread itself.
    for original, patched, what in ((THREAD_EXIT_ORIGINAL, THREAD_EXIT_PATCHED, "thread exit"),
                                    (THREAD_SPAWN_ORIGINAL, THREAD_SPAWN_PATCHED, "thread spawn"),
                                    (START_FAIL_ORIGINAL, START_FAIL_PATCHED, "start failure")):
        # The patched form keeps the original inside it, so it is looked for first.
        if patched in base_text:
            continue
        if base_text.count(original) != 1:
            raise RuntimeError(
                f"Unsupported WebRTC CoreAudio base: {what} anchor missing")
        base_text = base_text.replace(original, patched, 1)
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


def patch_aec3_state(source: Path):
    """AEC3's judgments told to the bridge and logged (the header above):
    the marked insertions of AEC3_INSERTIONS in echo_remover.cc and the
    subtractor."""
    aec3 = source / "modules" / "audio_processing" / "aec3"
    marked = re.compile(rf"^[ \t]*{re.escape(AEC3_BEGIN)}\n.*?^[ \t]*{re.escape(AEC3_END)}\n",
                        re.DOTALL | re.MULTILINE)
    for name in sorted({name for name, _, _, _ in AEC3_INSERTIONS}):
        path = aec3 / name
        # What an earlier run put in is taken out first, so that a second run
        # puts the current text in again.
        text = marked.sub("", path.read_text(encoding="utf-8"))
        if AEC3_BEGIN in text or AEC3_END in text:
            raise RuntimeError(f"Unsupported partial KSIP AEC3 state patch in {name}")
        for _, anchor, where, body in (i for i in AEC3_INSERTIONS if i[0] == name):
            if text.count(anchor) != 1:
                raise RuntimeError(f"Unsupported WebRTC AEC3 source: {name} anchor missing")
            indent = anchor[:len(anchor) - len(anchor.lstrip(" "))]
            block = f"{indent}{AEC3_BEGIN}\n{body}{indent}{AEC3_END}\n"
            text = text.replace(anchor, anchor + block if where == "after" else block + anchor, 1)
        path.write_text(text, encoding="utf-8")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    source = args.source.resolve()
    patch_raw_capture(source)
    patch_active_before_start(source)
    patch_silent_delivered(source)
    patch_aec3_state(source)
    bridge = source / "ksip_bridge"
    bridge.mkdir(exist_ok=True)
    for name in ["ksip_audio_bridge.h", "ksip_audio_bridge_internal.h",
                 "bridge_state.h", "callback_gate.h", "serving.h",
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
