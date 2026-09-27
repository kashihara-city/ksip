// The bridge's one object and what its parts share: the WebRTC device
// module and audio processing it holds, the callback slots the audio
// threads call through, the device names and the diagnostics. The parts
// are in files of their own: ksip_audio_bridge.cc (the C ABI: create,
// start, stop, detach, destroy), device_selection.cc (which endpoint the
// ADM opens), pcm_processing.cc (the frames the ADM hands over and asks
// for) and apm.cc (echo cancellation and its statistics).
#ifndef KSIP_AUDIO_BRIDGE_STATE_H_
#define KSIP_AUDIO_BRIDGE_STATE_H_

#include "ksip_audio_bridge.h"
#include "callback_gate.h"

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <mutex>
#include <optional>
#include <span>
#include <string>
#include <vector>

#include "api/audio/audio_device.h"
#include "api/audio/audio_device_defines.h"
#include "api/audio/audio_processing.h"
#include "api/environment/environment.h"
#include "api/environment/environment_factory.h"
#include "common_audio/resampler/include/push_resampler.h"
#include "rtc_base/win/scoped_com_initializer.h"

namespace ksip_audio_bridge {
constexpr uint32_t kSampleRate = 48000;
constexpr size_t kFrames10Ms = kSampleRate / 100;
constexpr double kLevelSmoothing = 0.99;
// The two entries WebRTC's Windows ADM puts in front of the endpoints when it
// names them: index 0 is the default device, index 1 the default
// communications device, each with the endpoint id of whatever holds that role.
constexpr int kRoleEntries = 2;

inline double FramePower(std::span<const int16_t> samples) {
  if (samples.empty()) return 0;
  double sum = 0;
  for (const int16_t sample : samples) {
    const double normalized = sample / 32768.0;
    sum += normalized * normalized;
  }
  return sum / samples.size();
}
inline void UpdatePower(double frame_power, uint64_t frame_count, double &power) {
  power = frame_count ? kLevelSmoothing * power +
                            (1.0 - kLevelSmoothing) * frame_power
                      : frame_power;
}
inline double PowerDbfs(double power) {
  return 10.0 * std::log10(std::max(power, 1e-10));
}
// apm.cc
bool AnyProcessing(const ksip_audio_processing &p);
webrtc::AudioProcessing::Config ApmConfig(const ksip_audio_processing &p);
}  // namespace ksip_audio_bridge

struct ksip_audio final : public webrtc::AudioTransport {
  ksip_audio(uint32_t fallback_delay, const ksip_audio_processing &p)
      : fallback_delay_ms(fallback_delay),
        processing(p),
        processing_enabled(ksip_audio_bridge::AnyProcessing(p)),
        com(webrtc::ScopedCOMInitializer::kMTA),
        env(webrtc::CreateEnvironment()),
        mono_config(ksip_audio_bridge::kSampleRate, 1) {}

  // ksip_audio_bridge.cc
  int Initialize();

  // device_selection.cc
  int SetDevice(const char *id, bool playout);
  int Select(bool playout, int index, const char *name, const char *guid);
  void StoreDevice(bool playout, const char *name, const char *id);

  // apm.cc
  int ProcessRenderApm(const int16_t *mono48, bool input_error = false);
  int ProcessCaptureApm(int16_t *mono48, uint32_t total_delay_ms,
      double device_frame_power, double mono_frame_power,
      uint32_t device_rate, uint32_t device_channels);
  void PollAgcMetrics();
  int ResetDiagnostics();

  // pcm_processing.cc
  int32_t NeedMorePlayData(size_t frames, size_t bytes, size_t channels,
      uint32_t rate, void *samples, size_t &frames_out, int64_t *elapsed,
      int64_t *ntp) override;
  void PullRenderData(int bits, int rate, size_t channels, size_t frames,
      void *samples, int64_t *elapsed, int64_t *ntp) override;
  int32_t RecordedDataIsAvailable(const void *samples, size_t frames,
      size_t bytes, size_t channels, uint32_t rate, uint32_t delay,
      int32_t, uint32_t, bool, uint32_t &) override;
  int32_t RecordedDataIsAvailable(const void *samples, size_t frames,
      size_t bytes, size_t channels, uint32_t rate, uint32_t delay,
      int32_t, uint32_t, bool, uint32_t &,
      std::optional<int64_t> capture_time_ns) override;
  int32_t ProcessCapture(const void *samples, size_t frames, size_t bytes,
      size_t channels, uint32_t rate, uint32_t total_delay_ms,
      std::optional<int64_t> capture_time_ns);

  const uint32_t fallback_delay_ms;
  const ksip_audio_processing processing;
  const bool processing_enabled;
  webrtc::ScopedCOMInitializer com;
  const webrtc::Environment env;
  const webrtc::StreamConfig mono_config;
  webrtc::scoped_refptr<webrtc::AudioProcessing> apm;
  webrtc::scoped_refptr<webrtc::AudioDeviceModule> adm;
  // The callbacks the audio threads call, each behind a gate: a detach
  // clears the slot and returns only when no call with the old argument is
  // still running, since the caller is about to free what it points to.
  ksip_audio_internal::CallbackGate<ksip_audio_render_cb> render_gate;
  ksip_audio_internal::CallbackGate<ksip_audio_capture_cb> capture_gate;
  std::mutex device_mutex;
  std::string recording_name;
  std::string recording_id;
  std::string playout_name;
  std::string playout_id;
  // What the last start asked for, as it asked ("default" included), so that
  // a start for the same endpoint can take over the running stream.
  std::string recording_request;
  std::string playout_request;
  std::mutex apm_mutex;
  std::vector<int16_t> reverse_scratch;
  webrtc::PushResampler<int16_t> render_resampler;
  webrtc::PushResampler<int16_t> capture_resampler;
  uint32_t last_stream_delay_ms = 0;
  bool last_stream_delay_from_device = false;
  uint64_t render_frames = 0;
  uint64_t capture_frames = 0;
  uint32_t render_errors = 0;
  uint32_t capture_errors = 0;
  double render_power = 0;
  double capture_device_power = 0;
  double capture_mono_power = 0;
  double capture_input_power = 0;
  double capture_output_power = 0;
  uint32_t capture_device_rate = 0;
  uint32_t capture_device_channels = 0;
  // The last AGC2 report, see PollAgcMetrics.
  bool agc_reported = false;
  double agc_speech_level_dbfs = 0;
  double agc_noise_level_dbfs = 0;
  double agc_headroom_db = 0;
  double agc_gain_db = 0;
};

#endif
