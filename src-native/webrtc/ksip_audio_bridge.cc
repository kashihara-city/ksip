#include "ksip_audio_bridge.h"
#include "ksip_audio_bridge_internal.h"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <string>
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

namespace {
constexpr uint32_t kSampleRate = 48000;
constexpr size_t kFrames10Ms = kSampleRate / 100;
constexpr double kLevelSmoothing = 0.99;

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

webrtc::AudioProcessing::Config ApmConfig(bool enabled,
                                           bool echo_enabled = true) {
  webrtc::AudioProcessing::Config config;
  config.echo_canceller.enabled = enabled && echo_enabled;
  config.high_pass_filter.enabled = enabled;
  config.noise_suppression.enabled = enabled;
  config.noise_suppression.level =
      webrtc::AudioProcessing::Config::NoiseSuppression::kHigh;
  config.gain_controller1.enabled = false;
  config.gain_controller2.enabled = enabled;
  config.gain_controller2.input_volume_controller.enabled = false;
  config.gain_controller2.adaptive_digital.enabled = enabled;
  return config;
}
}  // namespace

struct ksip_audio final : public webrtc::AudioTransport {
  ksip_audio(uint32_t fallback_delay, bool processing)
      : fallback_delay_ms(fallback_delay),
        processing_enabled(processing),
        com(webrtc::ScopedCOMInitializer::kMTA),
        env(webrtc::CreateEnvironment()),
        mono_config(kSampleRate, 1) {}

  int Initialize() {
    if (!com.Succeeded()) return -2;
    apm = webrtc::BuiltinAudioProcessingBuilder(
              ApmConfig(processing_enabled)).Build(env);
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
    const int count = playout ? adm->PlayoutDevices() : adm->RecordingDevices();
    for (int i = 0; i < count; ++i) {
      char name[webrtc::kAdmMaxDeviceNameSize] = {};
      char guid[webrtc::kAdmMaxGuidSize] = {};
      const int result = playout
          ? adm->PlayoutDeviceName(static_cast<uint16_t>(i), name, guid)
          : adm->RecordingDeviceName(static_cast<uint16_t>(i), name, guid);
      if (!result && std::strcmp(id, guid) == 0) {
        const int set_result = playout
            ? adm->SetPlayoutDevice(static_cast<uint16_t>(i))
            : adm->SetRecordingDevice(static_cast<uint16_t>(i));
        if (!set_result) StoreDevice(playout, name, guid);
        return set_result;
      }
    }
    return -5;
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
    ksip_audio_render_cb callback;
    void *arg;
    {
      std::lock_guard<std::mutex> lock(callback_mutex);
      callback = render_callback;
      arg = render_arg;
    }
    std::vector<int16_t> mono48(kFrames10Ms, 0);
    const bool callback_error =
        callback && callback(arg, mono48.data(), mono48.size());
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
    ksip_audio_capture_cb callback;
    void *arg;
    {
      std::lock_guard<std::mutex> lock(callback_mutex);
      callback = capture_callback;
      arg = capture_arg;
    }
    if (callback)
      callback(arg, mono48.data(), mono48.size(), capture_time_ns.value_or(0));
    return 0;
  }

  const uint32_t fallback_delay_ms;
  const bool processing_enabled;
  webrtc::ScopedCOMInitializer com;
  const webrtc::Environment env;
  const webrtc::StreamConfig mono_config;
  webrtc::scoped_refptr<webrtc::AudioProcessing> apm;
  webrtc::scoped_refptr<webrtc::AudioDeviceModule> adm;
  std::mutex callback_mutex;
  std::mutex device_mutex;
  std::string recording_name;
  std::string recording_id;
  std::string playout_name;
  std::string playout_id;
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
  audio->apm->ApplyConfig(ApmConfig(true, enabled));
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
extern "C" int ksip_audio_create(uint32_t delay, int processing,
                                  ksip_audio **out) {
  if (!out || delay > 500) return -1;
  *out = nullptr;
  auto audio = std::make_unique<ksip_audio>(delay, processing != 0);
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
  if (!audio || !callback || audio->adm->Playing()) return -1;
  if (!audio->adm->Recording() && audio->ResetDiagnostics()) return -4;
  if (audio->SetDevice(id, true) || audio->adm->InitPlayout()) return -2;
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
extern "C" void ksip_audio_stop_playout(ksip_audio *audio) {
  if (!audio) return;
  if (audio->adm->Playing()) audio->adm->StopPlayout();
  std::lock_guard<std::mutex> lock(audio->callback_mutex);
  audio->render_callback = nullptr; audio->render_arg = nullptr;
}
extern "C" int ksip_audio_start_recording(ksip_audio *audio, const char *id,
    ksip_audio_capture_cb callback, void *arg) {
  if (!audio || !callback || audio->adm->Recording()) return -1;
  if (!audio->adm->Playing() && audio->ResetDiagnostics()) return -4;
  if (audio->SetDevice(id, false) || audio->adm->InitRecording()) return -2;
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
  std::lock_guard<std::mutex> lock(audio->callback_mutex);
  audio->capture_callback = nullptr; audio->capture_arg = nullptr;
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
