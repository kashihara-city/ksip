#include "ksip_audio_bridge.h"
#include "ksip_audio_bridge_internal.h"

#include <atomic>
#include <algorithm>
#include <array>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <thread>

namespace {
std::atomic<unsigned> callbacks{0};
std::atomic<unsigned> capture_callbacks{0};
bool render_signal = false;
unsigned signal_position = 0;
uint32_t live_random = 0x41554344;
int32_t live_smooth = 0;

constexpr size_t kFrame = 480;
constexpr size_t kEchoDelayFrames = 8;

uint32_t NextRandom(uint32_t &state) {
  state ^= state << 13;
  state ^= state >> 17;
  state ^= state << 5;
  return state;
}

int TestSyntheticEcho(ksip_audio *audio) {
  std::array<std::array<int16_t, kFrame>, kEchoDelayFrames + 1> history{};
  std::array<int16_t, kFrame> render{};
  std::array<int16_t, kFrame> capture{};
  uint32_t random = 0x6b736970;
  double input_energy = 0;
  double output_energy = 0;
  size_t measured_samples = 0;
  ksip_audio_stats stats{};
  for (int block = 0; block < 3000; ++block) {
    const double envelope = 0.35 + 0.65 * std::pow(
        std::sin(3.141592653589793 * (block % 100) / 100.0), 2);
    int32_t smooth = 0;
    for (size_t i = 0; i < kFrame; ++i) {
      const int32_t noise =
          static_cast<int32_t>(NextRandom(random) & 0xffff) - 32768;
      smooth = (3 * smooth + noise) / 4;
      render[i] = static_cast<int16_t>(std::clamp(
          static_cast<int32_t>(smooth * envelope * 0.55), -16000, 16000));
    }
    history[block % history.size()] = render;
    const auto &echo = history[(block + 1) % history.size()];
    for (size_t i = 0; i < kFrame; ++i)
      capture[i] = static_cast<int16_t>(echo[i] * 0.45);
    const auto before = capture;
    if (ksip_audio_internal::ProcessSyntheticFrame(
            audio, render.data(), capture.data(), capture.size(),
            kEchoDelayFrames * 10))
      return 6;
    // Match the application's roughly 300 ms statistics polling interval.
    if ((block + 1) % 30 == 0 && ksip_audio_get_stats(audio, &stats)) return 7;
    if (block >= 1500) {
      for (size_t i = 0; i < kFrame; ++i) {
        input_energy += static_cast<double>(before[i]) * before[i];
        output_energy += static_cast<double>(capture[i]) * capture[i];
      }
      measured_samples += kFrame;
    }
  }
  if (ksip_audio_get_stats(audio, &stats)) return 7;
  const double input_rms = std::sqrt(input_energy / measured_samples);
  const double output_rms = std::sqrt(output_energy / measured_samples);
  const double suppression =
      20.0 * std::log10(input_rms / std::max(output_rms, 1e-9));
  std::printf(
      "Synthetic echo: input %.2f, output %.2f, suppression %.2f dB, "
      "ERL %.2f dB, ERLE %.2f dB, delay %d ms, stream delay %u ms, "
      "levels %.2f/%.2f/%.2f dBFS, frames %llu/%llu, errors %u/%u\n",
      input_rms, output_rms, suppression, stats.echo_return_loss,
      stats.echo_return_loss_enhancement, stats.delay_ms,
      stats.stream_delay_ms, stats.render_rms_dbfs,
      stats.capture_input_rms_dbfs, stats.capture_output_rms_dbfs,
      static_cast<unsigned long long>(stats.render_frames),
      static_cast<unsigned long long>(stats.capture_frames),
      stats.render_errors, stats.capture_errors);
  const uint32_t required = KSIP_AUDIO_STATS_ECHO_RETURN_LOSS |
      KSIP_AUDIO_STATS_ECHO_RETURN_LOSS_ENHANCEMENT |
      KSIP_AUDIO_STATS_DELAY | KSIP_AUDIO_STATS_RENDER_LEVEL |
      KSIP_AUDIO_STATS_CAPTURE_INPUT_LEVEL |
      KSIP_AUDIO_STATS_CAPTURE_OUTPUT_LEVEL;
  if ((stats.flags & required) != required || suppression < 10.0 ||
      stats.echo_return_loss_enhancement < 10.0 || stats.delay_ms < 40 ||
      stats.delay_ms > 100 || stats.stream_delay_ms != 80 ||
      !stats.stream_delay_from_device || stats.render_frames != 3000 ||
      stats.capture_frames != 3000 || stats.render_errors ||
      stats.capture_errors ||
      stats.capture_output_rms_dbfs > stats.capture_input_rms_dbfs - 10.0)
    return 8;
  return 0;
}

int RenderSilence(void *, int16_t *samples, size_t frames) {
  if (render_signal) {
    for (size_t i = 0; i < frames; ++i) {
      const int32_t noise =
          static_cast<int32_t>(NextRandom(live_random) & 0xffff) - 32768;
      live_smooth = (3 * live_smooth + noise) / 4;
      samples[i] = static_cast<int16_t>(live_smooth / 4);
      ++signal_position;
    }
  } else {
    std::memset(samples, 0, frames * sizeof(*samples));
  }
  callbacks.fetch_add(1, std::memory_order_relaxed);
  return 0;
}

void CaptureSamples(void *, const int16_t *, size_t, int64_t) {
  capture_callbacks.fetch_add(1, std::memory_order_relaxed);
}
}  // namespace

int main(int argc, char **argv) {
  if (ksip_audio_internal::InterleavedSamples(480, 2) != 960) return 3;
  ksip_audio *audio = nullptr;
  const ksip_audio_processing processing{1, 1, 2, 1};
  if (ksip_audio_create(20, &processing, &audio) != 0 || !audio) return 1;
  ksip_audio_stats stats{};
  if (ksip_audio_get_stats(audio, &stats) != 0) return 2;
  const bool disable_aec = argc >= 2 &&
      !std::strcmp(argv[1], "--duplex-no-aec");
  if (disable_aec) {
    if (ksip_audio_internal::SetEchoCancellationForTest(audio, false))
      return 15;
  } else {
    const int synthetic_result = TestSyntheticEcho(audio);
    if (synthetic_result) return synthetic_result;
  }
  const bool test_capture = argc >= 2 &&
      (!std::strcmp(argv[1], "--capture") ||
       !std::strcmp(argv[1], "--identify") ||
       !std::strcmp(argv[1], "--duplex-no-aec") ||
       !std::strcmp(argv[1], "--duplex-signal") ||
       !std::strcmp(argv[1], "--duplex-record-first"));
  if (test_capture) {
    const char *device = argc >= 3 ? argv[2] : "default";
    const bool duplex = std::strcmp(argv[1], "--capture") != 0;
    const bool record_first =
        !std::strcmp(argv[1], "--duplex-record-first");
    const char *playout_device = argc >= 4 ? argv[3] : "default";
    if (duplex && !record_first) {
      render_signal = true;
      if (ksip_audio_start_playout(
              audio, playout_device, RenderSilence, nullptr))
        return 12;
    }
    if (ksip_audio_start_recording(audio, device, CaptureSamples, nullptr))
      return 9;
    if (duplex && record_first) {
      render_signal = true;
      if (ksip_audio_start_playout(
              audio, playout_device, RenderSilence, nullptr))
        return 12;
    }
    ksip_audio_device_info device_info{};
    if (ksip_audio_get_device_info(audio, &device_info)) return 13;
    std::printf("Recording endpoint: %s | %s\n",
                device_info.recording_name, device_info.recording_id);
    if (std::strcmp(device, "default") &&
        std::strcmp(device, device_info.recording_id))
      return 14;
    if (!std::strcmp(argv[1], "--identify")) {
      std::this_thread::sleep_for(std::chrono::milliseconds(250));
      ksip_audio_stop_recording(audio);
      ksip_audio_destroy(audio);
      return 0;
    }
    for (int second = 1; second <= 5; ++second) {
      std::this_thread::sleep_for(std::chrono::seconds(1));
      if (ksip_audio_get_stats(audio, &stats)) return 10;
      std::printf(
          "Capture %d s: device %.2f, mono %.2f, input %.2f, output %.2f "
          "dBFS, ERL %.2f, ERLE %.2f, delay %d/%u ms, "
          "format %u Hz/%u ch, frames %llu, errors %u\n",
          second, stats.capture_device_rms_dbfs,
          stats.capture_mono_rms_dbfs, stats.capture_input_rms_dbfs,
          stats.capture_output_rms_dbfs, stats.echo_return_loss,
          stats.echo_return_loss_enhancement, stats.delay_ms,
          stats.stream_delay_ms, stats.capture_device_rate,
          stats.capture_device_channels,
          static_cast<unsigned long long>(stats.capture_frames),
          stats.capture_errors);
      std::fflush(stdout);
    }
    ksip_audio_stop_recording(audio);
    if (duplex) ksip_audio_stop_playout(audio);
    if (capture_callbacks.load(std::memory_order_relaxed) < 20) return 11;
  }
  const bool test_playout = argc == 2 &&
      (!std::strcmp(argv[1], "--playout") ||
       !std::strcmp(argv[1], "--playout-signal"));
  if (test_playout) {
    render_signal = !std::strcmp(argv[1], "--playout-signal");
    if (ksip_audio_start_playout(audio, "default", RenderSilence, nullptr))
      return 4;
    std::this_thread::sleep_for(std::chrono::milliseconds(
        render_signal ? 1200 : 750));
    ksip_audio_stop_playout(audio);
    if (callbacks.load(std::memory_order_relaxed) < 20) return 5;
  }
  ksip_audio_destroy(audio);
  return 0;
}
