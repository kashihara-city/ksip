// In-band DTMF: a digit sent as sound on the call it is for, in place of
// the microphone's sound, for a PBX that listens for the tones instead of
// taking RFC 4733 events or SIP INFO. The two frequencies of ITU-T Q.23 (the
// same as Japan's push-button table), each digit a 100 ms tone and a 100 ms
// pause, which meets the timing of MIC Notice No. 357 of 2024, Appended
// Table 2 (tone 50 ms or more, pause 30 ms or more, period 120 ms or more).
// The high tone is 2 dB above the low one, so that the low group never
// exceeds the high group, as that table requires. The sound itself is in
// inband_dtmf_tone.h.
#pragma once
#include <cstddef>
#include "inband_dtmf_tone.h"

struct audio;
struct auframe;

namespace inband_dtmf {
// How many digits may wait behind the one sounding.
constexpr size_t QUEUE_LIMIT = 32;
// Queues a digit (0-9, *, #, A-D) for the call whose audio stream this is.
// False for anything else, or when too many are waiting.
bool queue(const audio *stream, char digit);
// While a digit of this stream is due, its tone (or the pause after it)
// takes the place of the frame's samples; otherwise the frame is left alone.
void fill(const audio *stream, auframe *f);
// The stream's encoder is gone: what it had waiting goes with it.
void forget(const audio *stream);
} // namespace inband_dtmf
