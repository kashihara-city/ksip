// The frames the ADM asks for and hands over: the call's 48 kHz mono is
// resampled to what the device runs at and spread over its channels on the
// way out, and mixed down and resampled on the way in, with the echo
// canceller fed on both paths. The callbacks into the module go through
// the gates, so that a detach can wait for a call in flight.
#include "bridge_state.h"
#include "ksip_audio_bridge_internal.h"

#include <cstring>

using ksip_audio_bridge::FramePower;
using ksip_audio_bridge::kFrames10Ms;

int32_t ksip_audio::NeedMorePlayData(size_t frames, size_t bytes, size_t channels,
    uint32_t rate, void *samples, size_t &frames_out, int64_t *elapsed,
    int64_t *ntp) {
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
  // The in-flight lock comes first, then the callback is read: a detach
  // that cleared it before this point is not seen with a stale argument,
  // and one that clears it after waits for this call to end.
  render_gate.Invoke([&](ksip_audio_render_cb callback, void *arg) {
    callback_error = callback && callback(arg, mono48.data(), mono48.size());
  });
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

void ksip_audio::PullRenderData(int bits, int rate, size_t channels, size_t frames,
    void *samples, int64_t *elapsed, int64_t *ntp) {
  size_t frames_out = 0;
  NeedMorePlayData(frames, bits / 8, channels, rate, samples, frames_out,
                   elapsed, ntp);
}

int32_t ksip_audio::RecordedDataIsAvailable(const void *samples, size_t frames,
    size_t bytes, size_t channels, uint32_t rate, uint32_t delay,
    int32_t, uint32_t, bool, uint32_t &) {
  return ProcessCapture(samples, frames, bytes, channels, rate, delay,
                        std::nullopt);
}
int32_t ksip_audio::RecordedDataIsAvailable(const void *samples, size_t frames,
    size_t bytes, size_t channels, uint32_t rate, uint32_t delay,
    int32_t, uint32_t, bool, uint32_t &,
    std::optional<int64_t> capture_time_ns) {
  return ProcessCapture(samples, frames, bytes, channels, rate, delay,
                        capture_time_ns);
}

int32_t ksip_audio::ProcessCapture(const void *samples, size_t frames, size_t bytes,
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
  capture_gate.Invoke([&](ksip_audio_capture_cb callback, void *arg) {
    if (callback)
      callback(arg, mono48.data(), mono48.size(), capture_time_ns.value_or(0));
  });
  return 0;
}
