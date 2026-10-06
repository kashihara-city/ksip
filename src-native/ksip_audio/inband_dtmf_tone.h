// The sound of an in-band DTMF digit, apart from any call: which two
// frequencies, how loud, how long. Needs nothing of baresip, so that the unit
// test holds the tone against the rules itself (inband_dtmf.h says which).
#pragma once
#include <cmath>
#include <cstdint>
#include <cstring>

namespace inband_dtmf {
constexpr unsigned TONE_MS = 100;
constexpr unsigned PAUSE_MS = 100;
constexpr unsigned MS_PER_S = 1000;
// Each tone at a level telephones use, relative to a full-scale sine (about
// +3.14 dBm0): the low one at -10 dBm0, the high one 2 dB above it.
inline const double LOW_TONE_GAIN = std::pow(10.0, (-10.0 - 3.14) / 20.0);
inline const double HIGH_TONE_GAIN = std::pow(10.0, (-8.0 - 3.14) / 20.0);

// The low and high frequencies of a digit (0-9, *, #, A-D), in Hz; false
// when it is not one.
inline bool frequencies(char digit, double &low, double &high) {
    static const char keys[] = "123A456B789C*0#D";
    static const double lows[] = {697, 770, 852, 941};
    static const double highs[] = {1209, 1336, 1477, 1633};
    const char *at = digit ? std::strchr(keys, digit) : nullptr;
    if (!at) return false;
    const auto index = at - keys;
    low = lows[index / 4];
    high = highs[index % 4];
    return true;
}
// Samples (per channel) of a digit's tone, and of its tone and pause.
inline uint64_t tone_samples(unsigned srate) { return static_cast<uint64_t>(srate) * TONE_MS / MS_PER_S; }
inline uint64_t period_samples(unsigned srate) { return tone_samples(srate) + static_cast<uint64_t>(srate) * PAUSE_MS / MS_PER_S; }
// The digit's sound `position` samples into its period, full scale being 1:
// the two sines during the tone, silence during the pause.
inline double sample(char digit, uint64_t position, unsigned srate) {
    double low = 0, high = 0;
    if (!srate || position >= tone_samples(srate) || !frequencies(digit, low, high)) return 0;
    constexpr double PI = 3.14159265358979323846;
    const double t = static_cast<double>(position) / srate;
    return LOW_TONE_GAIN * std::sin(2 * PI * low * t) + HIGH_TONE_GAIN * std::sin(2 * PI * high * t);
}
} // namespace inband_dtmf
