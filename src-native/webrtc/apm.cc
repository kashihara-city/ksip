// The echo canceller and what goes with it: how the settings become an
// APM configuration, the render and capture passes with their levels and
// delays, the AGC2 reports WebRTC only gives through its histograms, the
// statistics the app shows, and the two hooks the synthetic-echo test uses.
#include "bridge_state.h"
#include "ksip_audio_bridge_internal.h"

#include <map>
#include <memory>

#include "system_wrappers/include/metrics.h"

using ksip_audio_bridge::FramePower;
using ksip_audio_bridge::kFrames10Ms;
using ksip_audio_bridge::kSampleRate;
using ksip_audio_bridge::PowerDbfs;
using ksip_audio_bridge::UpdatePower;

namespace ksip_audio_bridge {
namespace {
webrtc::AudioProcessing::Config::NoiseSuppression::Level NoiseLevel(int level) {
  using NS = webrtc::AudioProcessing::Config::NoiseSuppression;
  switch (level) {
    case 0: return NS::kLow;
    case 1: return NS::kModerate;
    case 3: return NS::kVeryHigh;
    default: return NS::kHigh;
  }
}
}  // namespace

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
}  // namespace ksip_audio_bridge

int ksip_audio::ProcessRenderApm(const int16_t *mono48, bool input_error) {
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

int ksip_audio::ProcessCaptureApm(int16_t *mono48, uint32_t total_delay_ms,
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
void ksip_audio::PollAgcMetrics() {
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

int ksip_audio::ResetDiagnostics() {
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
  audio->apm->ApplyConfig(ksip_audio_bridge::ApmConfig(processing));
  return audio->apm->Initialize();
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
