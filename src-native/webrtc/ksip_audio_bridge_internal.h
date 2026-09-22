#ifndef KSIP_AUDIO_BRIDGE_INTERNAL_H_
#define KSIP_AUDIO_BRIDGE_INTERNAL_H_

#include <stddef.h>
#include <stdint.h>

struct ksip_audio;

namespace ksip_audio_internal {

// AudioDeviceBuffer expects nSamplesOut to contain the total number of
// interleaved samples, although nSamples is expressed per channel.
inline size_t InterleavedSamples(size_t frames, size_t channels) {
  return frames * channels;
}

// Drives the same render/capture APM path as the ADM without opening a device.
// This is intentionally kept out of the public C ABI and exists for the
// deterministic synthetic-echo test.
int ProcessSyntheticFrame(ksip_audio *audio, const int16_t *render,
                          int16_t *capture, size_t frames,
                          uint32_t stream_delay_ms);

// Keeps the production NS/AGC2 configuration while disabling only AEC so a
// physical-loopback test can attribute the measured suppression correctly.
int SetEchoCancellationForTest(ksip_audio *audio, bool enabled);

}  // namespace ksip_audio_internal

#endif
