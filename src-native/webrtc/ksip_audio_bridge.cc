#include "ksip_audio_bridge.h"
#include "ksip_audio_bridge_internal.h"

#include <algorithm>
#include <atomic>
#include <cmath>
#include <cstring>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <string>
#include <thread>
#include <vector>

#include "api/audio/audio_device.h"
#include "api/audio/audio_device_defines.h"
#include "api/audio/audio_processing.h"
#include "api/audio/builtin_audio_processing_builder.h"
#include "api/environment/environment.h"
#include "api/environment/environment_factory.h"
#include "common_audio/resampler/include/push_resampler.h"
#include "modules/audio_device/include/audio_device_factory.h"
#include "rtc_base/logging.h"
#include "rtc_base/win/scoped_com_initializer.h"
#include "system_wrappers/include/metrics.h"

namespace {
constexpr uint32_t kSampleRate = 48000;
constexpr size_t kFrames10Ms = kSampleRate / 100;
constexpr double kLevelSmoothing = 0.99;
// The two entries WebRTC's Windows ADM puts in front of the endpoints when it
// names them: index 0 is the default device, index 1 the default
// communications device, each with the endpoint id of whatever holds that role.
constexpr int kRoleEntries = 2;

double FramePower(std::span<const int16_t> samples) {
  if (samples.empty()) return 0;
  double sum = 0;
  for (const int16_t sample : samples) {
    const double normalized = sample / 32768.0;
    sum += normalized * normalized;
  }
  return sum / samples.size();
}

void UpdatePower(double frame_power, uint64_t frame_count, double &power) {
  power = frame_count ? kLevelSmoothing * power +
                            (1.0 - kLevelSmoothing) * frame_power
                      : frame_power;
}

double PowerDbfs(double power) {
  return 10.0 * std::log10(std::max(power, 1e-10));
}

webrtc::AudioProcessing::Config::NoiseSuppression::Level NoiseLevel(int level) {
  using NS = webrtc::AudioProcessing::Config::NoiseSuppression;
  switch (level) {
    case 0: return NS::kLow;
    case 1: return NS::kModerate;
    case 3: return NS::kVeryHigh;
    default: return NS::kHigh;
  }
}

bool AnyProcessing(const ksip_audio_processing &p) {
  return p.echo_cancellation || p.high_pass_filter ||
         p.noise_suppression >= 0 || p.gain_control;
}

webrtc::AudioProcessing::Config ApmConfig(const ksip_audio_processing &p) {
  webrtc::AudioProcessing::Config config;
  config.echo_canceller.enabled = p.echo_cancellation != 0;
  // The high-pass filter is a setting of its own; AEC3 would otherwise turn
  // it on whenever it runs.
  config.echo_canceller.enforce_high_pass_filtering = false;
  config.high_pass_filter.enabled = p.high_pass_filter != 0;
  config.noise_suppression.enabled = p.noise_suppression >= 0;
  config.noise_suppression.level = NoiseLevel(p.noise_suppression);
  config.gain_controller1.enabled = false;
  config.gain_controller2.enabled = p.gain_control != 0;
  config.gain_controller2.input_volume_controller.enabled = false;
  config.gain_controller2.adaptive_digital.enabled = p.gain_control != 0;
  return config;
}
}  // namespace

struct ksip_audio final : public webrtc::AudioTransport {
  ksip_audio(uint32_t fallback_delay, const ksip_audio_processing &p)
      : fallback_delay_ms(fallback_delay),
        processing(p),
        processing_enabled(AnyProcessing(p)),
        com(webrtc::ScopedCOMInitializer::kMTA),
        env(webrtc::CreateEnvironment()),
        mono_config(kSampleRate, 1) {}

  int Initialize() {
    if (!com.Succeeded()) return -2;
    apm = webrtc::BuiltinAudioProcessingBuilder(
              ApmConfig(processing)).Build(env);
    if (!apm) return -3;
    adm = webrtc::CreateWindowsCoreAudioAudioDeviceModule(env);
    if (!adm || adm->Init() || adm->RegisterAudioCallback(this)) return -4;
    return 0;
  }

  int SetDevice(const char *id, bool playout) {
    if (!id || !id[0] || std::strcmp(id, "default") == 0) {
      const int result = playout
          ? adm->SetPlayoutDevice(webrtc::AudioDeviceModule::kDefaultCommunicationDevice)
          : adm->SetRecordingDevice(webrtc::AudioDeviceModule::kDefaultCommunicationDevice);
      if (result) return result;
      char name[webrtc::kAdmMaxDeviceNameSize] = {};
      char guid[webrtc::kAdmMaxGuidSize] = {};
      const int name_result = playout
          ? adm->PlayoutDeviceName(static_cast<uint16_t>(-1), name, guid)
          : adm->RecordingDeviceName(static_cast<uint16_t>(-1), name, guid);
      if (!name_result) StoreDevice(playout, name, guid);
      return 0;
    }
    // PlayoutDevices() and RecordingDevices() count the endpoints, but the
    // list that PlayoutDeviceName() and RecordingDeviceName() index has the
    // two role entries in front of them. A loop over the count alone never
    // reached the last two endpoints, so a chosen device that was neither a
    // Windows default nor early in the list counted as not there, and the
    // call failed with no device. The endpoints themselves are matched first,
    // so that a chosen device stays chosen when Windows moves its defaults;
    // a role entry only serves as a fallback.
    const int count = playout ? adm->PlayoutDevices() : adm->RecordingDevices();
    const int enumerated = count > 0 ? count + kRoleEntries : 0;
    int role_index = -1;
    std::string listing;
    for (int i = 0; i < enumerated; ++i) {
      char name[webrtc::kAdmMaxDeviceNameSize] = {};
      char guid[webrtc::kAdmMaxGuidSize] = {};
      const int result = playout
          ? adm->PlayoutDeviceName(static_cast<uint16_t>(i), name, guid)
          : adm->RecordingDeviceName(static_cast<uint16_t>(i), name, guid);
      if (result) continue;
      listing += " [" + std::to_string(i) + "] " + name + " " + guid;
      if (std::strcmp(id, guid) != 0) continue;
      if (i < kRoleEntries) {
        if (role_index < 0) role_index = i;
        continue;
      }
      return Select(playout, i, name, guid);
    }
    if (role_index >= 0) {
      char name[webrtc::kAdmMaxDeviceNameSize] = {};
      char guid[webrtc::kAdmMaxGuidSize] = {};
      const int result = playout
          ? adm->PlayoutDeviceName(static_cast<uint16_t>(role_index), name, guid)
          : adm->RecordingDeviceName(static_cast<uint16_t>(role_index), name, guid);
      if (!result) return Select(playout, role_index, name, guid);
    }
    // Written at warning level, which the app's log always carries, so that a
    // device the engine cannot find is explained next to the failure.
    RTC_LOG(LS_WARNING) << "ksip_audio: " << (playout ? "playout" : "recording")
                        << " device " << id << " is not among the " << count
                        << " endpoints WebRTC lists:" << listing;
    return -5;
  }

  int Select(bool playout, int index, const char *name, const char *guid) {
    const int result = playout
        ? adm->SetPlayoutDevice(static_cast<uint16_t>(index))
        : adm->SetRecordingDevice(static_cast<uint16_t>(index));
    if (result) {
      RTC_LOG(LS_WARNING) << "ksip_audio: selecting "
                          << (playout ? "playout" : "recording") << " device ["
                          << index << "] " << name << " failed (" << result << ")";
      return result;
    }
    StoreDevice(playout, name, guid);
    return 0;
  }

  void StoreDevice(bool playout, const char *name, const char *id) {
    std::lock_guard<std::mutex> lock(device_mutex);
    auto &stored_name = playout ? playout_name : recording_name;
    auto &stored_id = playout ? playout_id : recording_id;
    stored_name = name ? name : "";
    stored_id = id ? id : "";
  }

  int ProcessRenderApm(const int16_t *mono48, bool input_error = false) {
    if (!processing_enabled) return 0;
    std::lock_guard<std::mutex> lock(apm_mutex);
    if (input_error) ++render_errors;
    UpdatePower(FramePower({mono48, kFrames10Ms}), render_frames,
                render_power);
    ++render_frames;
    reverse_scratch.resize(kFrames10Ms);
    const int result = apm->ProcessReverseStream(
        mono48, mono_config, mono_config, reverse_scratch.data());
    if (result != webrtc::AudioProcessing::kNoError) ++render_errors;
    return result;
  }

  int ProcessCaptureApm(int16_t *mono48, uint32_t total_delay_ms,
      double device_frame_power, double mono_frame_power,
      uint32_t device_rate, uint32_t device_channels) {
    if (!processing_enabled) return 0;
    const uint32_t delay = std::min(
        total_delay_ms ? total_delay_ms : fallback_delay_ms, 500u);
    std::lock_guard<std::mutex> lock(apm_mutex);
    UpdatePower(device_frame_power, capture_frames, capture_device_power);
    UpdatePower(mono_frame_power, capture_frames, capture_mono_power);
    UpdatePower(FramePower({mono48, kFrames10Ms}), capture_frames,
                capture_input_power);
    capture_device_rate = device_rate;
    capture_device_channels = device_channels;
    ++capture_frames;
    last_stream_delay_ms = delay;
    last_stream_delay_from_device = total_delay_ms != 0;
    const int delay_result = apm->set_stream_delay_ms(delay);
    if (delay_result != webrtc::AudioProcessing::kNoError &&
        delay_result != webrtc::AudioProcessing::kBadStreamParameterWarning) {
      ++capture_errors;
      return delay_result;
    }
    const int result =
        apm->ProcessStream(mono48, mono_config, mono_config, mono48);
    if (result != webrtc::AudioProcessing::kNoError) {
      ++capture_errors;
      return result;
    }
    UpdatePower(FramePower({mono48, kFrames10Ms}), capture_frames - 1,
                capture_output_power);
    return result;
  }

  // Takes what AGC2 reported since the last poll. It reports every 10 s of
  // processing through WebRTC's histograms (there is no statistics field for
  // it), so a poll sees one report at most; the value seen most is kept.
  void PollAgcMetrics() {
    std::map<std::string, std::unique_ptr<webrtc::metrics::SampleInfo>,
             webrtc::AbslStringViewCmp> histograms;
    webrtc::metrics::GetAndReset(&histograms);
    auto reported = [&](const char *name, double scale, double *out) {
      const auto it = histograms.find(name);
      if (it == histograms.end() || it->second->samples.empty()) return false;
      int value = 0, events = 0;
      for (const auto &[sample, count] : it->second->samples)
        if (count >= events) { value = sample; events = count; }
      *out = scale * value;
      return true;
    };
    double speech = 0, noise = 0, headroom = 0, gain = 0;
    const bool got_speech =
        reported("WebRTC.Audio.Agc2.EstimatedSpeechLevel", -1.0, &speech);
    const bool got_noise =
        reported("WebRTC.Audio.Agc2.EstimatedNoiseLevel", -1.0, &noise);
    const bool got_headroom =
        reported("WebRTC.Audio.Agc2.Headroom", 1.0, &headroom);
    if (reported("WebRTC.Audio.Agc2.DigitalGainApplied", 1.0, &gain)) {
      agc_reported = true;
      agc_gain_db = gain;
      if (got_speech) agc_speech_level_dbfs = speech;
      if (got_noise) agc_noise_level_dbfs = noise;
      if (got_headroom) agc_headroom_db = headroom;
    }
  }

  int ResetDiagnostics() {
    std::lock_guard<std::mutex> lock(apm_mutex);
    const int result = apm->Initialize();
    last_stream_delay_ms = 0;
    last_stream_delay_from_device = false;
    render_frames = 0;
    capture_frames = 0;
    render_errors = 0;
    capture_errors = 0;
    render_power = 0;
    capture_input_power = 0;
    capture_output_power = 0;
    capture_device_power = 0;
    capture_mono_power = 0;
    capture_device_rate = 0;
    capture_device_channels = 0;
    agc_reported = false;
    return result;
  }

  int32_t RecordedDataIsAvailable(const void *samples, size_t frames,
      size_t bytes, size_t channels, uint32_t rate, uint32_t delay,
      int32_t, uint32_t, bool, uint32_t &) override {
    return ProcessCapture(samples, frames, bytes, channels, rate, delay,
                          std::nullopt);
  }
  int32_t RecordedDataIsAvailable(const void *samples, size_t frames,
      size_t bytes, size_t channels, uint32_t rate, uint32_t delay,
      int32_t, uint32_t, bool, uint32_t &,
      std::optional<int64_t> capture_time_ns) override {
    return ProcessCapture(samples, frames, bytes, channels, rate, delay,
                          capture_time_ns);
  }

  int32_t NeedMorePlayData(size_t frames, size_t bytes, size_t channels,
      uint32_t rate, void *samples, size_t &frames_out, int64_t *elapsed,
      int64_t *ntp) override {
    frames_out = 0;
    if (elapsed) *elapsed = -1;
    if (ntp) *ntp = -1;
    if (!samples || bytes != channels * sizeof(int16_t) ||
        frames * 100 != rate || rate < 8000 || !channels || channels > 2) {
      if (samples) std::memset(samples, 0, frames * bytes);
      return -1;
    }
    frames_out = ksip_audio_internal::InterleavedSamples(frames, channels);
    std::vector<int16_t> mono48(kFrames10Ms, 0);
    bool callback_error = false;
    {
      // The in-flight lock comes first, then the callback is read: a detach
      // that cleared it before this point is not seen with a stale argument,
      // and one that clears it after waits for this call to end.
      std::lock_guard<std::mutex> in_flight(render_call_mutex);
      render_call_thread.store(std::this_thread::get_id());
      ksip_audio_render_cb callback;
      void *arg;
      {
        std::lock_guard<std::mutex> lock(callback_mutex);
        callback = render_callback;
        arg = render_arg;
      }
      callback_error = callback && callback(arg, mono48.data(), mono48.size());
      render_call_thread.store(std::thread::id{});
    }
    if (callback_error)
      std::fill(mono48.begin(), mono48.end(), 0);

    std::vector<int16_t> device(frames);
    render_resampler.Resample(webrtc::MonoView<const int16_t>(mono48),
                              webrtc::MonoView<int16_t>(device));
    std::span<int16_t> output(static_cast<int16_t *>(samples), frames * channels);
    for (size_t frame = 0; frame < frames; ++frame)
      for (size_t channel = 0; channel < channels; ++channel)
        output[frame * channels + channel] = device[frame];

    // A reverse-stream processing error must not suppress local playout.
    (void)ProcessRenderApm(mono48.data(), callback_error);
    return 0;
  }

  void PullRenderData(int bits, int rate, size_t channels, size_t frames,
      void *samples, int64_t *elapsed, int64_t *ntp) override {
    size_t frames_out = 0;
    NeedMorePlayData(frames, bits / 8, channels, rate, samples, frames_out,
                     elapsed, ntp);
  }

  int32_t ProcessCapture(const void *samples, size_t frames, size_t bytes,
      size_t channels, uint32_t rate, uint32_t total_delay_ms,
      std::optional<int64_t> capture_time_ns) {
    if (!samples || bytes != channels * sizeof(int16_t) ||
        frames * 100 != rate || rate < 8000 || !channels || channels > 2)
      return -1;
    std::span<const int16_t> input(static_cast<const int16_t *>(samples),
                                   frames * channels);
    const double device_frame_power = FramePower(input);
    std::vector<int16_t> mono_device(frames);
    for (size_t frame = 0; frame < frames; ++frame) {
      int32_t sum = 0;
      for (size_t channel = 0; channel < channels; ++channel)
        sum += input[frame * channels + channel];
      mono_device[frame] = static_cast<int16_t>(sum / channels);
    }
    std::vector<int16_t> mono48(kFrames10Ms);
    capture_resampler.Resample(webrtc::MonoView<const int16_t>(mono_device),
                               webrtc::MonoView<int16_t>(mono48));
    const double mono_frame_power = FramePower(mono_device);
    const int process_result = ProcessCaptureApm(
        mono48.data(), total_delay_ms, device_frame_power, mono_frame_power,
        rate, static_cast<uint32_t>(channels));
    if (process_result != webrtc::AudioProcessing::kNoError)
      return process_result;
    {
      std::lock_guard<std::mutex> in_flight(capture_call_mutex);
      capture_call_thread.store(std::this_thread::get_id());
      ksip_audio_capture_cb callback;
      void *arg;
      {
        std::lock_guard<std::mutex> lock(callback_mutex);
        callback = capture_callback;
        arg = capture_arg;
      }
      if (callback)
        callback(arg, mono48.data(), mono48.size(), capture_time_ns.value_or(0));
      capture_call_thread.store(std::thread::id{});
    }
    return 0;
  }

  const uint32_t fallback_delay_ms;
  const ksip_audio_processing processing;
  const bool processing_enabled;
  webrtc::ScopedCOMInitializer com;
  const webrtc::Environment env;
  const webrtc::StreamConfig mono_config;
  webrtc::scoped_refptr<webrtc::AudioProcessing> apm;
  webrtc::scoped_refptr<webrtc::AudioDeviceModule> adm;
  std::mutex callback_mutex;
  // Held by the audio thread for the whole of a callback. A detach clears
  // the callback under callback_mutex and then takes this lock once, so that
  // it returns only when no call with the old argument is still running; the
  // caller is about to free what that argument points to. The thread id is
  // kept so that a detach from inside the callback itself does not wait on
  // its own lock.
  std::mutex render_call_mutex;
  std::mutex capture_call_mutex;
  std::atomic<std::thread::id> render_call_thread{};
  std::atomic<std::thread::id> capture_call_thread{};
  std::mutex device_mutex;
  std::string recording_name;
  std::string recording_id;
  std::string playout_name;
  std::string playout_id;
  // What the last start asked for, as it asked ("default" included), so that
  // a start for the same endpoint can take over the running stream.
  std::string recording_request;
  std::string playout_request;
  ksip_audio_render_cb render_callback = nullptr;
  void *render_arg = nullptr;
  ksip_audio_capture_cb capture_callback = nullptr;
  void *capture_arg = nullptr;
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

int ksip_audio_internal::ProcessSyntheticFrame(ksip_audio *audio,
    const int16_t *render, int16_t *capture, size_t frames,
    uint32_t stream_delay_ms) {
  if (!audio || !render || !capture || frames != kFrames10Ms ||
      !audio->processing_enabled)
    return -1;
  const int reverse_result = audio->ProcessRenderApm(render);
  if (reverse_result != webrtc::AudioProcessing::kNoError)
    return reverse_result;
  const double power = FramePower({capture, frames});
  return audio->ProcessCaptureApm(capture, stream_delay_ms, power, power,
                                  kSampleRate, 1);
}

int ksip_audio_internal::SetEchoCancellationForTest(ksip_audio *audio,
                                                      bool enabled) {
  if (!audio || !audio->processing_enabled) return -1;
  std::lock_guard<std::mutex> lock(audio->apm_mutex);
  ksip_audio_processing processing = audio->processing;
  processing.echo_cancellation = enabled;
  audio->apm->ApplyConfig(ApmConfig(processing));
  return audio->apm->Initialize();
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
    // A stream kept running on the same endpoint is taken over as it is; a
    // different endpoint means the device really changes.
    bool same;
    {
      std::lock_guard<std::mutex> lock(audio->device_mutex);
      same = audio->playout_request == request;
    }
    if (same) {
      std::lock_guard<std::mutex> lock(audio->callback_mutex);
      audio->render_callback = callback; audio->render_arg = arg;
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
  {
    std::lock_guard<std::mutex> lock(audio->callback_mutex);
    audio->render_callback = callback; audio->render_arg = arg;
  }
  if (audio->adm->StartPlayout()) {
    std::lock_guard<std::mutex> lock(audio->callback_mutex);
    audio->render_callback = nullptr; audio->render_arg = nullptr;
    return -3;
  }
  return 0;
}
namespace {
// Clears a callback and returns once no call of it is still running, so that
// the caller may free the argument. Called from the callback's own thread it
// only clears, since the call in flight is the caller itself.
void ClearAndDrain(std::mutex &callback_mutex, std::mutex &call_mutex,
    std::atomic<std::thread::id> &call_thread, auto &callback, void *&arg) {
  {
    std::lock_guard<std::mutex> lock(callback_mutex);
    callback = nullptr; arg = nullptr;
  }
  if (call_thread.load() == std::this_thread::get_id()) return;
  std::lock_guard<std::mutex> in_flight(call_mutex);
}
}  // namespace
extern "C" void ksip_audio_stop_playout(ksip_audio *audio) {
  if (!audio) return;
  if (audio->adm->Playing()) audio->adm->StopPlayout();
  ClearAndDrain(audio->callback_mutex, audio->render_call_mutex, audio->render_call_thread,
                audio->render_callback, audio->render_arg);
}
extern "C" void ksip_audio_detach_playout(ksip_audio *audio) {
  if (!audio) return;
  // Without a callback the stream renders silence until the next start. The
  // caller frees the argument next, so a callback running right now is
  // waited for before this returns.
  ClearAndDrain(audio->callback_mutex, audio->render_call_mutex, audio->render_call_thread,
                audio->render_callback, audio->render_arg);
}
extern "C" int ksip_audio_playout_running(ksip_audio *audio) {
  return audio && audio->adm->Playing();
}
extern "C" int ksip_audio_start_recording(ksip_audio *audio, const char *id,
    ksip_audio_capture_cb callback, void *arg) {
  if (!audio || !callback) return -1;
  const std::string request = id && id[0] ? id : "default";
  if (audio->adm->Recording()) {
    bool same;
    {
      std::lock_guard<std::mutex> lock(audio->device_mutex);
      same = audio->recording_request == request;
    }
    if (same) {
      std::lock_guard<std::mutex> lock(audio->callback_mutex);
      audio->capture_callback = callback; audio->capture_arg = arg;
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
  {
    std::lock_guard<std::mutex> lock(audio->callback_mutex);
    audio->capture_callback = callback; audio->capture_arg = arg;
  }
  if (audio->adm->StartRecording()) {
    std::lock_guard<std::mutex> lock(audio->callback_mutex);
    audio->capture_callback = nullptr; audio->capture_arg = nullptr;
    return -3;
  }
  return 0;
}
extern "C" void ksip_audio_stop_recording(ksip_audio *audio) {
  if (!audio) return;
  if (audio->adm->Recording()) audio->adm->StopRecording();
  ClearAndDrain(audio->callback_mutex, audio->capture_call_mutex, audio->capture_call_thread,
                audio->capture_callback, audio->capture_arg);
}
extern "C" void ksip_audio_detach_recording(ksip_audio *audio) {
  if (!audio) return;
  // Without a callback the captured frames are dropped until the next start.
  // As with the playout, the callback in flight is waited for.
  ClearAndDrain(audio->callback_mutex, audio->capture_call_mutex, audio->capture_call_thread,
                audio->capture_callback, audio->capture_arg);
}
extern "C" int ksip_audio_recording_running(ksip_audio *audio) {
  return audio && audio->adm->Recording();
}
extern "C" int ksip_audio_get_stats(ksip_audio *audio,
                                     ksip_audio_stats *stats) {
  if (!audio || !stats || !audio->processing_enabled) return -1;
  std::lock_guard<std::mutex> lock(audio->apm_mutex);
  *stats = {};
  stats->stream_delay_ms = audio->last_stream_delay_ms;
  stats->stream_delay_from_device = audio->last_stream_delay_from_device;
  stats->render_frames = audio->render_frames;
  stats->capture_frames = audio->capture_frames;
  stats->render_errors = audio->render_errors;
  stats->capture_errors = audio->capture_errors;
  stats->capture_device_rate = audio->capture_device_rate;
  stats->capture_device_channels = audio->capture_device_channels;
  if (audio->processing.gain_control) {
    audio->PollAgcMetrics();
    if (audio->agc_reported) {
      stats->agc_speech_level_dbfs = audio->agc_speech_level_dbfs;
      stats->agc_noise_level_dbfs = audio->agc_noise_level_dbfs;
      stats->agc_headroom_db = audio->agc_headroom_db;
      stats->agc_gain_db = audio->agc_gain_db;
      stats->flags |= KSIP_AUDIO_STATS_AGC;
    }
  }
  if (audio->render_frames) {
    stats->render_rms_dbfs = PowerDbfs(audio->render_power);
    stats->flags |= KSIP_AUDIO_STATS_RENDER_LEVEL;
  }
  if (audio->capture_frames) {
    stats->capture_device_rms_dbfs = PowerDbfs(audio->capture_device_power);
    stats->capture_mono_rms_dbfs = PowerDbfs(audio->capture_mono_power);
    stats->capture_input_rms_dbfs = PowerDbfs(audio->capture_input_power);
    stats->capture_output_rms_dbfs = PowerDbfs(audio->capture_output_power);
    stats->flags |= KSIP_AUDIO_STATS_CAPTURE_DEVICE_LEVEL |
                    KSIP_AUDIO_STATS_CAPTURE_MONO_LEVEL |
                    KSIP_AUDIO_STATS_CAPTURE_INPUT_LEVEL |
                    KSIP_AUDIO_STATS_CAPTURE_OUTPUT_LEVEL;
  }
  const auto apm_stats = audio->apm->GetStatistics();
  if (apm_stats.echo_return_loss) {
    stats->echo_return_loss = *apm_stats.echo_return_loss;
    stats->flags |= KSIP_AUDIO_STATS_ECHO_RETURN_LOSS;
  }
  if (apm_stats.echo_return_loss_enhancement) {
    stats->echo_return_loss_enhancement =
        *apm_stats.echo_return_loss_enhancement;
    stats->flags |= KSIP_AUDIO_STATS_ECHO_RETURN_LOSS_ENHANCEMENT;
  }
  if (apm_stats.delay_ms) {
    stats->delay_ms = *apm_stats.delay_ms;
    stats->flags |= KSIP_AUDIO_STATS_DELAY;
  }
  if (apm_stats.divergent_filter_fraction) {
    stats->divergent_filter_fraction =
        *apm_stats.divergent_filter_fraction;
    stats->flags |= KSIP_AUDIO_STATS_DIVERGENT_FILTER_FRACTION;
  }
  if (apm_stats.delay_median_ms) {
    stats->delay_median_ms = *apm_stats.delay_median_ms;
    stats->flags |= KSIP_AUDIO_STATS_DELAY_MEDIAN;
  }
  if (apm_stats.delay_standard_deviation_ms) {
    stats->delay_standard_deviation_ms =
        *apm_stats.delay_standard_deviation_ms;
    stats->flags |= KSIP_AUDIO_STATS_DELAY_STANDARD_DEVIATION;
  }
  if (apm_stats.residual_echo_likelihood) {
    stats->residual_echo_likelihood =
        *apm_stats.residual_echo_likelihood;
    stats->flags |= KSIP_AUDIO_STATS_RESIDUAL_ECHO_LIKELIHOOD;
  }
  if (apm_stats.residual_echo_likelihood_recent_max) {
    stats->residual_echo_likelihood_recent_max =
        *apm_stats.residual_echo_likelihood_recent_max;
    stats->flags |= KSIP_AUDIO_STATS_RESIDUAL_ECHO_LIKELIHOOD_RECENT_MAX;
  }
  return 0;
}

extern "C" int ksip_audio_get_device_info(ksip_audio *audio,
                                            ksip_audio_device_info *info) {
  if (!audio || !info) return -1;
  std::lock_guard<std::mutex> lock(audio->device_mutex);
  *info = {};
  const auto copy = [](char *destination, const std::string &source) {
    std::strncpy(destination, source.c_str(), KSIP_AUDIO_DEVICE_TEXT_SIZE - 1);
  };
  copy(info->recording_name, audio->recording_name);
  copy(info->recording_id, audio->recording_id);
  copy(info->playout_name, audio->playout_name);
  copy(info->playout_id, audio->playout_id);
  return 0;
}
