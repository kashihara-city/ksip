// Unit tests of the in-band DTMF queue (ksip_audio/inband_dtmf.cpp): which
// digits it takes, how they are laid over the frames a call sends, and that
// each call keeps its own. The sound of one digit is test-ksip-audio's.
#include <re.h>
#include <rem.h>
#include "ksip_audio/inband_dtmf.h"
#include <cmath>
#include <cstdio>
#include <string>
#include <vector>

namespace {
int failures = 0;
void check(bool ok, const char *what) {
    std::printf("%s: %s\n", ok ? "PASS" : "FAIL", what);
    if (!ok) ++failures;
}
// Streams are told apart by address only; any two distinct addresses do.
int first_call = 0, second_call = 0;
const audio *call_a = reinterpret_cast<const audio *>(&first_call);
const audio *call_b = reinterpret_cast<const audio *>(&second_call);
constexpr int16_t MIC = 1234;
// A frame as baresip hands one to a filter (auframe_init without libre).
auframe frame_of(void *samples, size_t count, enum aufmt fmt, unsigned srate, unsigned ch) {
    auframe f{};
    f.fmt = fmt;
    f.sampv = samples;
    f.sampc = count;
    f.srate = srate;
    f.ch = uint8_t(ch);
    return f;
}

// What a call would send: the digits in turn, each its period, then the
// microphone again (samples of one channel, 16-bit).
std::vector<int16_t> expected(const std::string &digits, unsigned srate, size_t total) {
    std::vector<int16_t> out;
    for (char d : digits)
        for (uint64_t i = 0; i < inband_dtmf::period_samples(srate); ++i) out.push_back(int16_t(std::lrint(inband_dtmf::sample(d, i, srate) * 32767.0)));
    out.resize(std::max(out.size(), total), MIC);
    out.resize(total);
    return out;
}
// Sends `total` samples (per channel) of microphone sound through fill, in
// frames of `frame` samples, and gives back what went out on each channel.
std::vector<std::vector<int16_t>> send(const audio *stream, unsigned srate, unsigned ch, size_t frame, size_t total) {
    std::vector<std::vector<int16_t>> out(ch);
    for (size_t done = 0; done < total; done += frame) {
        const size_t n = std::min(frame, total - done);
        std::vector<int16_t> samples(n * ch, MIC);
        auto f = frame_of(samples.data(), samples.size(), AUFMT_S16LE, srate, ch);
        inband_dtmf::fill(stream, &f);
        for (size_t i = 0; i < samples.size(); ++i) out[i % ch].push_back(samples[i]);
    }
    return out;
}

void only_keypad_digits_are_taken_and_no_more_than_the_limit() {
    check(!inband_dtmf::queue(call_a, 'x') && !inband_dtmf::queue(call_a, 0) && !inband_dtmf::queue(nullptr, '5'), "anything but a digit, or no call, is refused");
    bool all = true;
    for (size_t i = 0; i < inband_dtmf::QUEUE_LIMIT; ++i) all = all && inband_dtmf::queue(call_a, '1');
    check(all && !inband_dtmf::queue(call_a, '1'), "digits are taken up to the limit, and the one after is refused");
    // One taken out to sound leaves room for one more.
    send(call_a, 8000, 1, 80, 80);
    check(inband_dtmf::queue(call_a, '1') && !inband_dtmf::queue(call_a, '1'), "a digit that starts sounding makes room for one");
    inband_dtmf::forget(call_a);
}
void nothing_queued_leaves_the_microphone_alone() {
    const auto out = send(call_a, 48000, 1, 960, 4800);
    check(out[0] == std::vector<int16_t>(4800, MIC), "with no digit the frames are the microphone's, untouched");
}
void digits_follow_one_another_across_frames_of_any_size() {
    for (unsigned srate : {8000u, 16000u, 48000u})
        for (size_t frame : {size_t(srate / 100), size_t(srate / 50), size_t(77), size_t(1)}) {
            inband_dtmf::queue(call_a, '1');
            inband_dtmf::queue(call_a, '#');
            inband_dtmf::queue(call_a, '0');
            const size_t total = 3 * inband_dtmf::period_samples(srate) + srate / 10;
            const auto out = send(call_a, srate, 1, frame, total);
            char what[160];
            std::snprintf(what, sizeof what, "%u Hz in frames of %zu: each digit its tone and pause in turn, no gap, no overlap, then the microphone mid-frame", srate, frame);
            check(out[0] == expected("1#0", srate, total), what);
        }
}
void every_channel_and_float_frames_carry_the_tone() {
    inband_dtmf::queue(call_a, '9');
    const size_t total = inband_dtmf::period_samples(16000) + 320;
    const auto out = send(call_a, 16000, 2, 320, total);
    check(out[0] == expected("9", 16000, total) && out[1] == out[0], "both channels of a stereo frame carry the same tone");
    inband_dtmf::queue(call_a, '9');
    std::vector<float> samples(inband_dtmf::period_samples(16000), 0.5f);
    auto f = frame_of(samples.data(), samples.size(), AUFMT_FLOAT, 16000, 1);
    inband_dtmf::fill(call_a, &f);
    bool same = true;
    for (size_t i = 0; i < samples.size(); ++i) same = same && std::fabs(samples[i] - float(inband_dtmf::sample('9', i, 16000))) < 1e-6f;
    check(same, "a float frame gets the tone as it is, without rounding to 16 bits");
    std::vector<int16_t> odd(160, MIC);
    auto other = frame_of(odd.data(), odd.size(), AUFMT_S24_3LE, 16000, 1);
    inband_dtmf::queue(call_b, '9');
    inband_dtmf::fill(call_b, &other);
    check(odd == std::vector<int16_t>(160, MIC), "a frame of a format it does not write is left alone");
    inband_dtmf::forget(call_b);
}
void each_call_sounds_its_own_digits() {
    inband_dtmf::queue(call_a, '1');
    inband_dtmf::queue(call_b, '9');
    inband_dtmf::queue(call_b, '*');
    const size_t total = 2 * inband_dtmf::period_samples(8000) + 160;
    std::vector<int16_t> a, b;
    for (size_t done = 0; done < total; done += 160) {
        // The two calls' frames interleaved, as two calls up at once send them.
        auto sa = send(call_a, 8000, 1, 160, 160), sb = send(call_b, 8000, 1, 160, 160);
        a.insert(a.end(), sa[0].begin(), sa[0].end());
        b.insert(b.end(), sb[0].begin(), sb[0].end());
    }
    a.resize(total);
    b.resize(total);
    check(a == expected("1", 8000, total) && b == expected("9*", 8000, total), "two calls each send their own digits, in their own order");
}
void a_call_that_ends_takes_its_digits_with_it() {
    inband_dtmf::queue(call_a, '1');
    inband_dtmf::queue(call_a, '2');
    inband_dtmf::queue(call_a, '3');
    send(call_a, 8000, 1, 160, 160);
    inband_dtmf::forget(call_a);
    // A later call may be given the same address: it starts with nothing.
    const auto after = send(call_a, 8000, 1, 160, 3 * inband_dtmf::period_samples(8000));
    check(after[0] == std::vector<int16_t>(3 * inband_dtmf::period_samples(8000), MIC), "the digits of a call that ended never sound on the next");
    inband_dtmf::queue(call_a, '4');
    const size_t total = inband_dtmf::period_samples(8000);
    check(send(call_a, 8000, 1, 160, total)[0] == expected("4", 8000, total), "a digit for the next call starts from its beginning");
}
} // namespace

int main() {
    only_keypad_digits_are_taken_and_no_more_than_the_limit();
    nothing_queued_leaves_the_microphone_alone();
    digits_follow_one_another_across_frames_of_any_size();
    every_channel_and_float_frames_carry_the_tone();
    each_call_sounds_its_own_digits();
    a_call_that_ends_takes_its_digits_with_it();
    std::printf("PASS: %d failure(s)\n", failures);
    return failures ? 1 : 0;
}
