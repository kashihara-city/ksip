#include "inband_dtmf.h"
#include <re.h>
#include <rem.h>
#include <deque>
#include <map>
#include <mutex>

namespace inband_dtmf {
namespace {
struct Sounding {
    std::deque<char> waiting;
    char digit = 0;
    // Samples (per channel) into the digit's tone and pause.
    uint64_t position = 0;
};
// Digits are queued on the engine's thread and sounded on the audio thread.
std::mutex gate;
std::map<const audio *, Sounding> streams;
} // namespace

bool queue(const audio *stream, char digit) {
    double low = 0, high = 0;
    if (!stream || !frequencies(digit, low, high)) return false;
    std::lock_guard<std::mutex> lock(gate);
    auto &s = streams[stream];
    if (s.waiting.size() >= QUEUE_LIMIT) return false;
    s.waiting.push_back(digit);
    return true;
}
void fill(const audio *stream, auframe *f) {
    if (!f || !f->sampv || !f->srate || !f->ch || (f->fmt != AUFMT_S16LE && f->fmt != AUFMT_FLOAT)) return;
    std::lock_guard<std::mutex> lock(gate);
    auto found = streams.find(stream);
    if (found == streams.end()) return;
    auto &s = found->second;
    const size_t frames = f->sampc / f->ch;
    for (size_t i = 0; i < frames; ++i) {
        if (!s.digit) {
            // The last pause ended within this frame: the rest is the microphone's.
            if (s.waiting.empty()) break;
            s.digit = s.waiting.front();
            s.waiting.pop_front();
            s.position = 0;
        }
        const double value = sample(s.digit, s.position, f->srate);
        for (size_t c = 0; c < f->ch; ++c) {
            const size_t j = i * f->ch + c;
            if (f->fmt == AUFMT_S16LE) static_cast<int16_t *>(f->sampv)[j] = int16_t(std::lrint(value * 32767.0));
            else static_cast<float *>(f->sampv)[j] = float(value);
        }
        if (++s.position >= period_samples(f->srate)) s.digit = 0;
    }
    if (!s.digit && s.waiting.empty()) streams.erase(found);
}
void forget(const audio *stream) {
    std::lock_guard<std::mutex> lock(gate);
    streams.erase(stream);
}
} // namespace inband_dtmf
