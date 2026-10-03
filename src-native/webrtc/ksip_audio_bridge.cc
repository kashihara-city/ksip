// The bridge's C ABI: the module creates one bridge on the settings, starts
// and stops the playout and the recording on an endpoint, detaches the
// callbacks it gave, and destroys the bridge. The parts behind it are in
// device_selection.cc, pcm_processing.cc and apm.cc; bridge_state.h holds
// what they share.
#include "bridge_state.h"

#include <atomic>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>

#include "api/audio/builtin_audio_processing_builder.h"
#include "modules/audio_device/include/audio_device_factory.h"
#include "modules/audio_device/win/core_audio_utility_win.h"
#include "rtc_base/logging.h"
#include "system_wrappers/include/metrics.h"

namespace {
// For the capture and the playout: whether its stream is to take RAW mode,
// what its properties were last set to, and how the last stream that opened
// was opened (0 not yet, 1 RAW, 2 without it). A stream can take RAW mode in
// its properties and still fail to open (Windows' privacy settings refuse
// the microphone at Initialize), so it counts only once it has opened.
struct RawMode {
  std::atomic<bool> wanted{true};
  std::atomic<int> noted{0};
  std::atomic<int> taken{0};
};
RawMode capture_raw, playout_raw;
// The endpoint the last capture and the last playout stream opened, whichever
// way it was chosen (OpenedEndpoint), made once and never destroyed, as
// WebRTC builds without exit-time destructors.
struct Endpoint {
  std::mutex mutex;
  std::string id;
};
Endpoint& opened_endpoint(bool capture) {
  static Endpoint* const capture_side = new Endpoint();
  static Endpoint* const playout_side = new Endpoint();
  return capture ? *capture_side : *playout_side;
}
}  // namespace

void webrtc::webrtc_win::core_audio_utility::KsipNoteDevice(bool capture, const std::string& id) {
  Endpoint& endpoint = opened_endpoint(capture);
  std::lock_guard<std::mutex> lock(endpoint.mutex);
  endpoint.id = id;
}
std::string ksip_audio_bridge::OpenedEndpoint(bool playout) {
  Endpoint& endpoint = opened_endpoint(!playout);
  std::lock_guard<std::mutex> lock(endpoint.mutex);
  return endpoint.id;
}

// Asked and told by WebRTC's CoreAudio as a stream gets its properties and
// once it has opened (the RAW patch in scripts/build/patch-webrtc.py).
bool webrtc::webrtc_win::core_audio_utility::KsipWantRaw(bool capture) {
  return (capture ? capture_raw : playout_raw).wanted;
}
void webrtc::webrtc_win::core_audio_utility::KsipNoteRaw(bool capture, bool raw) {
  (capture ? capture_raw : playout_raw).noted = raw ? 1 : 2;
}
void webrtc::webrtc_win::core_audio_utility::KsipNoteOpened(bool capture) {
  RawMode& mode = capture ? capture_raw : playout_raw;
  mode.taken = mode.noted.load();
}

extern "C" void ksip_audio_set_raw(int capture, int playout) {
  capture_raw.wanted = capture != 0;
  playout_raw.wanted = playout != 0;
}
extern "C" int ksip_audio_capture_raw(void) {
  return capture_raw.taken;
}
extern "C" int ksip_audio_playout_raw(void) {
  return playout_raw.taken;
}

int ksip_audio::Initialize() {
  if (!com.Succeeded()) return -2;
  apm = webrtc::BuiltinAudioProcessingBuilder(
            ksip_audio_bridge::ApmConfig(processing)).Build(env);
  if (!apm) return -3;
  adm = webrtc::CreateWindowsCoreAudioAudioDeviceModule(env);
  if (!adm || adm->Init() || adm->RegisterAudioCallback(this)) return -4;
  return 0;
}

extern "C" void ksip_audio_set_log_level(int detail) {
  // WebRTC writes to stderr from LS_INFO by default, which the app reads as the
  // engine's log. Warnings explain a device or processing failure and are worth
  // keeping; the rest is noise unless detail was asked for. The configuration
  // is taken on its first use, so this runs before the audio device exists.
  const webrtc::LoggingSeverity level =
      detail ? webrtc::LS_INFO : webrtc::LS_WARNING;
  webrtc::LoggingConfig config;
  config.set_min_severity(level);
  config.set_debug_severity(level);
  webrtc::InitializeLogging(std::move(config));
}
extern "C" int ksip_audio_create(uint32_t delay,
                                  const ksip_audio_processing *processing,
                                  ksip_audio **out) {
  if (!out || !processing || delay > 500) return -1;
  *out = nullptr;
  // AGC2 reports its levels and gain through WebRTC's histograms, which
  // collect nothing until enabled: once per process, before WebRTC runs.
  static bool metrics_enabled = false;
  if (!metrics_enabled) {
    webrtc::metrics::Enable();
    metrics_enabled = true;
  }
  auto audio = std::make_unique<ksip_audio>(delay, *processing);
  const int result = audio->Initialize();
  if (result) return result;
  *out = audio.release();
  return 0;
}
extern "C" void ksip_audio_destroy(ksip_audio *audio) {
  if (!audio) return;
  ksip_audio_stop_recording(audio);
  ksip_audio_stop_playout(audio);
  audio->adm->RegisterAudioCallback(nullptr);
  audio->adm->Terminate();
  delete audio;
}
extern "C" int ksip_audio_start_playout(ksip_audio *audio, const char *id,
    ksip_audio_render_cb callback, void *arg) {
  if (!audio || !callback) return -1;
  const std::string request = id && id[0] ? id : "default";
  if (audio->adm->Playing()) {
    // A stream kept running for the same request is taken over as it is
    // while it serves it as a new one would (ksip_audio::Serves); another
    // request, or a device that is back, means the stream opens again.
    bool same;
    {
      std::lock_guard<std::mutex> lock(audio->device_mutex);
      same = audio->playout_request == request;
    }
    if (same && audio->Serves(request, true)) {
      audio->render_gate.Set(callback, arg);
      return 0;
    }
    ksip_audio_stop_playout(audio);
  }
  if (!audio->adm->Recording() && audio->ResetDiagnostics()) return -4;
  if (audio->SetDevice(id, true)) return -5;
  if (audio->adm->InitPlayout()) return -2;
  {
    std::lock_guard<std::mutex> lock(audio->device_mutex);
    audio->playout_request = request;
  }
  audio->render_gate.Set(callback, arg);
  if (audio->adm->StartPlayout()) {
    audio->render_gate.Set(nullptr, nullptr);
    return -3;
  }
  return 0;
}
extern "C" void ksip_audio_stop_playout(ksip_audio *audio) {
  if (!audio) return;
  if (audio->adm->Playing()) audio->adm->StopPlayout();
  audio->render_gate.ClearAndDrain();
}
extern "C" void ksip_audio_detach_playout(ksip_audio *audio) {
  if (!audio) return;
  // Without a callback the stream renders silence until the next start. The
  // caller frees the argument next, so a callback running right now is
  // waited for before this returns.
  audio->render_gate.ClearAndDrain();
}
extern "C" int ksip_audio_playout_running(ksip_audio *audio) {
  return audio && audio->adm->Playing();
}
extern "C" int ksip_audio_start_recording(ksip_audio *audio, const char *id,
    ksip_audio_capture_cb callback, void *arg) {
  if (!audio || !callback) return -1;
  const std::string request = id && id[0] ? id : "default";
  if (audio->adm->Recording()) {
    // As with the playout.
    bool same;
    {
      std::lock_guard<std::mutex> lock(audio->device_mutex);
      same = audio->recording_request == request;
    }
    if (same && audio->Serves(request, false)) {
      audio->capture_gate.Set(callback, arg);
      return 0;
    }
    ksip_audio_stop_recording(audio);
  }
  if (!audio->adm->Playing() && audio->ResetDiagnostics()) return -4;
  if (audio->SetDevice(id, false)) return -5;
  if (audio->adm->InitRecording()) return -2;
  {
    std::lock_guard<std::mutex> lock(audio->device_mutex);
    audio->recording_request = request;
  }
  audio->capture_gate.Set(callback, arg);
  if (audio->adm->StartRecording()) {
    audio->capture_gate.Set(nullptr, nullptr);
    return -3;
  }
  return 0;
}
extern "C" void ksip_audio_stop_recording(ksip_audio *audio) {
  if (!audio) return;
  if (audio->adm->Recording()) audio->adm->StopRecording();
  audio->capture_gate.ClearAndDrain();
}
extern "C" void ksip_audio_detach_recording(ksip_audio *audio) {
  if (!audio) return;
  // Without a callback the captured frames are dropped until the next start.
  // As with the playout, the callback in flight is waited for.
  audio->capture_gate.ClearAndDrain();
}
extern "C" int ksip_audio_recording_running(ksip_audio *audio) {
  return audio && audio->adm->Recording();
}
