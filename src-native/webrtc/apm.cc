// The echo canceller and what goes with it: how the settings become an
// APM configuration, the render and capture passes with their levels and
// delays, the AGC2 reports WebRTC only gives through its histograms, the
// statistics the app shows, and the two hooks the synthetic-echo test uses.
#include "bridge_state.h"
#include "ksip_audio_bridge_internal.h"

#include <atomic>
#include <cstdint>
#include <map>
#include <memory>
#include <optional>
#include <string>
#include <utility>

#include "api/audio/echo_canceller3_config.h"
#include "api/environment/environment_factory.h"
#include "api/field_trials.h"
#include "rtc_base/logging.h"
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

// AEC3 as KSIP runs it, the same for every device. Until its linear filter
// is usable, AEC3 suppresses by an assumed echo path gain, 1.0 by default:
// as if the far end came back as loud as it is played. The near end's speech
// under the far end is then cut, and quiet speech is cut to pieces, until
// AEC3 either finds the echo or decides the device has none (transparent
// mode). Its legacy rule for that waits for 6 s of loud far end without
// convergence, which takes 15 s and more of a call. So:
//  - the HMM classifier of transparent mode (field trial
//    WebRTC-Aec3TransparentModeHmm) in place of the legacy rule: it decides
//    after about 2 s of far end with no echo found, and leaves as soon as
//    the filter converges;
//  - an assumed gain of 0.1 (an amplitude: the echo 20 dB under the far end;
//    WebRTC's own field trials WebRTC-Aec3UseLow*ReflectionsDefaultGain set
//    the same): the near end is cut far less meanwhile. A strong echo is
//    heard more in the first 2 s or so, until the linear filter takes it,
//    which it does as soon as with 1.0.
webrtc::Environment Aec3Environment() {
  auto trials =
      webrtc::FieldTrials::Create("WebRTC-Aec3TransparentModeHmm/Enabled/");
  if (!trials) {
    RTC_LOG(LS_ERROR) << "AEC3: the HMM transparent mode could not be set; "
                         "the legacy one runs";
    return webrtc::CreateEnvironment();
  }
  return webrtc::CreateEnvironment(std::move(trials));
}

webrtc::EchoCanceller3Config Aec3Config() {
  webrtc::EchoCanceller3Config config;
  config.ep_strength.default_gain = 0.1f;
  return config;
}

namespace {
// What AEC3 judged of each block (4 ms), as the patched echo_remover.cc tells
// it from the capture thread (KsipNoteAec3, scripts/build/patch-webrtc.py):
// its two modes as of the last block, and the last block each of the rest
// was judged at (0: not since the reset). The app asks every 0.5 s, and at
// times in between, while AEC3 judges 125 times in 0.5 s; what is told is
// what it judged over the last second, so that none falls between two asks.
// One canceller runs in the process, and a reset of the processing starts it
// over (ResetAec3Judgments).
constexpr uint64_t kAec3WindowBlocks = 250;
struct Aec3Seen {
  std::atomic<uint64_t> blocks{0};
  std::atomic<bool> transparent{false};
  std::atomic<bool> linear{false};
  std::atomic<uint64_t> converged{0};
  std::atomic<uint64_t> learning{0};
  std::atomic<uint64_t> saturated_echo{0};
  std::atomic<uint64_t> diverged{0};
};
Aec3Seen aec3_seen;
}  // namespace

std::optional<uint32_t> Aec3Judgments() {
  const uint64_t blocks = aec3_seen.blocks;
  if (!blocks) return std::nullopt;
  const auto recent = [blocks](const std::atomic<uint64_t> &at) {
    const uint64_t block = at;
    return block && blocks - block < kAec3WindowBlocks;
  };
  uint32_t bits = 0;
  if (aec3_seen.transparent) bits |= KSIP_AUDIO_AEC3_TRANSPARENT;
  if (aec3_seen.linear) bits |= KSIP_AUDIO_AEC3_LINEAR;
  if (recent(aec3_seen.converged)) bits |= KSIP_AUDIO_AEC3_CONVERGED;
  if (recent(aec3_seen.learning)) bits |= KSIP_AUDIO_AEC3_LEARNING;
  if (recent(aec3_seen.saturated_echo)) bits |= KSIP_AUDIO_AEC3_SATURATED_ECHO;
  if (recent(aec3_seen.diverged)) bits |= KSIP_AUDIO_AEC3_DIVERGED;
  return bits;
}

void ResetAec3Judgments() {
  aec3_seen.blocks = 0;
  aec3_seen.transparent = false;
  aec3_seen.linear = false;
  aec3_seen.converged = 0;
  aec3_seen.learning = 0;
  aec3_seen.saturated_echo = 0;
  aec3_seen.diverged = 0;
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

void webrtc::KsipNoteAec3(bool transparent, bool linear, bool converged,
                          bool learning, bool saturated_echo, bool diverged) {
  using ksip_audio_bridge::aec3_seen;
  const uint64_t block = aec3_seen.blocks + 1;
  aec3_seen.transparent = transparent;
  aec3_seen.linear = linear;
  if (converged) aec3_seen.converged = block;
  if (learning) aec3_seen.learning = block;
  if (saturated_echo) aec3_seen.saturated_echo = block;
  if (diverged) aec3_seen.diverged = block;
  // Last, so that a reader that sees the block sees what was judged of it.
  aec3_seen.blocks = block;
}

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
  ksip_audio_bridge::ResetAec3Judgments();
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
  const int result = audio->apm->Initialize();
  ksip_audio_bridge::ResetAec3Judgments();
  return result;
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
  if (const auto aec3 = ksip_audio_bridge::Aec3Judgments()) {
    stats->aec3 = *aec3;
    stats->flags |= KSIP_AUDIO_STATS_AEC3;
  }
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
