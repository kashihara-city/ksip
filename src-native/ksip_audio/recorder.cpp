// The WAV recording; see recorder.h.
#include "recorder.h"
#include <algorithm>
#include <cmath>
#include <filesystem>
#include <string_view>
#include <chrono>
#include <climits>
#include <array>
#include <cstdio>
#include <cstddef>
#include <cstdint>
#include <string>
#include <mutex>

namespace recording_session {
namespace {
// The WAV header: its length, the "RIFF" tag and the size field in front of
// the rest, the fmt chunk's length, the PCM format tag, and the sample.
constexpr uint32_t HEADER_BYTES = 44, RIFF_PREFIX = 8, FMT_CHUNK_BYTES = 16, FORMAT_PCM = 1, BITS_PER_SAMPLE = 16, BYTES_PER_SAMPLE = 2;
// The most data a WAV can hold (its sizes are 32 bits), the clock's unit,
// and the slack as a fraction of a second.
constexpr uint64_t MAX_DATA_BYTES = 0xffff0000ULL, US_PER_S = 1000000;
constexpr uint32_t SLACK_FRACTION = 5;
bool put(FILE *f, const void *data, size_t n) { return fwrite(data, 1, n, f) == n; }
template <size_t N> bool tag(FILE *f, const char (&text)[N]) { return put(f, text, N - 1); }
bool le16(FILE *f, uint16_t n) {
    const unsigned char b[2] = {static_cast<unsigned char>(n), static_cast<unsigned char>(n >> CHAR_BIT)};
    return put(f, b, sizeof b);
}
bool le32(FILE *f, uint32_t n) { return le16(f, static_cast<uint16_t>(n)) && le16(f, static_cast<uint16_t>(n >> (2 * CHAR_BIT))); }
} // namespace

// The RIFF/WAVE header, written once the file opens and again with the
// final sizes when it closes: 16-bit PCM, two channels. False when a write
// failed.
bool Recorder::header() {
    constexpr uint32_t frame_bytes = channels * BYTES_PER_SAMPLE;
    return fseek(file, 0, SEEK_SET) == 0 && tag(file, "RIFF") && le32(file, HEADER_BYTES - RIFF_PREFIX + static_cast<uint32_t>(bytes)) &&
           tag(file, "WAVEfmt ") && le32(file, FMT_CHUNK_BYTES) && le16(file, FORMAT_PCM) && le16(file, static_cast<uint16_t>(channels)) &&
           le32(file, rate) && le32(file, rate * frame_bytes) && le16(file, static_cast<uint16_t>(frame_bytes)) && le16(file, BITS_PER_SAMPLE) &&
           tag(file, "data") && le32(file, static_cast<uint32_t>(bytes));
}
Recorder::Recorder(const std::string &path, uint32_t sr) : rate(sr), file_path(path), started(std::chrono::steady_clock::now()), slack(sr / SLACK_FRACTION) {
    // The path comes as UTF-8; read as such (u8path, which did the same, is deprecated in C++20).
    const std::u8string_view utf8(reinterpret_cast<const char8_t *>(path.data()), path.size());
    file = _wfopen(std::filesystem::path(utf8).c_str(), L"wb");
    if (!file) return;
    writing = true;
    if (!header()) failed = true;
    worker = std::thread([this] { run(); });
}
bool Recorder::writable() const {
    return ((remote.count != 0u) && (local.count != 0u)) || remote.count > slack || local.count > slack;
}
uint64_t Recorder::clock_frames() const {
    const auto passed = std::chrono::duration_cast<std::chrono::microseconds>(std::chrono::steady_clock::now() - started).count();
    return static_cast<uint64_t>(passed) * rate / US_PER_S;
}
int16_t Recorder::take(Side &side) {
    const int16_t v = side.ring[side.rd];
    side.rd = (side.rd + 1) % side.ring.size();
    --side.count;
    return v;
}
void Recorder::run() {
    std::array<int16_t, BLOCK_SAMPLES> buf{}; // frames of two channels
    for (;;) {
        size_t n = 0;
        {
            std::unique_lock<std::mutex> lock(mutex);
            wake.wait(lock, [this] { return done || writable(); });
            if (done && !remote.count && !local.count) break;
            // The two sides pair up as far as both have come. A side alone
            // waits the slack for the other, then goes with silence on the
            // other channel; at the end, whatever is left goes that way.
            bool take_far = false, take_near = false;
            const size_t far_have = remote.count, near_have = local.count;
            if (far_have && near_have) {
                n = std::min(far_have, near_have);
                take_far = take_near = true;
            } else if (far_have) {
                n = done ? far_have : far_have > slack ? far_have - slack : 0;
                take_far = true;
                take_near = false;
            } else {
                n = done ? near_have : near_have > slack ? near_have - slack : 0;
                take_far = false;
                take_near = true;
            }
            if (!n) continue;
            n = std::min(n, buf.size() / 2);
            for (size_t i = 0; i < n; ++i) {
                buf[2 * i] = take_far ? take(remote) : static_cast<int16_t>(0);
                buf[2 * i + 1] = take_near ? take(local) : static_cast<int16_t>(0);
            }
            written += n;
        }
        if (bytes + n * channels * BYTES_PER_SAMPLE > MAX_DATA_BYTES || fwrite(buf.data(), BYTES_PER_SAMPLE, channels * n, file) != channels * n) {
            failed = true;
            break;
        }
        bytes += n * channels * BYTES_PER_SAMPLE;
    }
    if (!header() || ferror(file) || fflush(file) != 0) failed = true;
}
// Into the side's ring, under the lock; what does not fit is counted. First
// the side is placed on the clock: the frames it has accounted for (written
// or in hand) are compared with the frames the clock has counted, and a
// shortfall beyond the slack goes into the ring as silence, in stream
// order: after what is in hand, before these samples. Arrival jitter stays
// within the slack and moves nothing; a real gap is filled to the clock, so
// that the side comes back where the other side is by then. Samples that
// were dropped for want of room are made up the same way. The silence never
// takes more than half the ring, so that it cannot crowd the samples out;
// a gap longer than that is placed short by the difference.
void Recorder::append(Side &side, const int16_t *samples, size_t frames) {
    const std::scoped_lock lock(mutex);
    if (done) return;
    const uint64_t accounted = written + side.count, now = clock_frames();
    size_t gap = now > accounted + slack ? static_cast<size_t>(now - accounted) : 0;
    gap = std::min(gap, side.ring.size() / 2);
    for (size_t i = 0; i < gap && side.count < side.ring.size(); ++i) {
        side.ring[side.wr] = 0;
        side.wr = (side.wr + 1) % side.ring.size();
        ++side.count;
    }
    for (size_t i = 0; i < frames; ++i) {
        if (side.count == side.ring.size()) {
            dropped += frames - i;
            break;
        }
        side.ring[side.wr] = samples[i];
        side.wr = (side.wr + 1) % side.ring.size();
        ++side.count;
    }
    if (writable()) wake.notify_one();
}
// Samples at `in_rate` into the side, at the file's rate. At the file's rate
// they go as they are; otherwise each output sample is placed between the
// two input samples around its time and interpolated, continuing across
// pushes (the last input sample of a push is kept for the first output of
// the next). A change of the input rate starts the placing afresh: one
// sample's worth of discontinuity, where the codec change itself is heard.
void Recorder::push(Side &side, const int16_t *in, size_t frames, uint32_t in_rate) {
    if (!writing || !frames || !in_rate) return;
    if (in_rate != side.rate) {
        side.rate = in_rate;
        side.pos = 0;
        side.last = 0;
    }
    if (in_rate == rate) {
        append(side, in, frames);
        return;
    }
    const double step = static_cast<double>(in_rate) / rate, end = static_cast<double>(frames) - 1.0;
    for (;;) {
        size_t n = 0;
        while (n < side.scratch.size() && side.pos < end) {
            const long idx = static_cast<long>(std::floor(side.pos));
            const double frac = side.pos - idx;
            const int a = idx < 0 ? side.last : in[idx], b = in[idx + 1];
            side.scratch[n++] = static_cast<int16_t>(std::lrint(a + (b - a) * frac));
            side.pos += step;
        }
        if (n) append(side, side.scratch.data(), n);
        if (side.pos >= end) break;
    }
    side.pos -= static_cast<double>(frames);
    side.last = in[frames - 1];
}
void Recorder::push_far(const int16_t *samples, size_t frames, uint32_t in_rate) { push(remote, samples, frames, in_rate); }
void Recorder::push_near(const int16_t *samples, size_t frames, uint32_t in_rate) { push(local, samples, frames, in_rate); }
Recorder::Summary Recorder::finish() {
    if (!finished) {
        finished = true;
        {
            const std::scoped_lock lock(mutex);
            done = true;
        }
        wake.notify_one();
        if (worker.joinable()) worker.join();
        if (file) {
            if (fclose(file) != 0) failed = true;
            file = nullptr;
        }
    }
    return Summary{bytes, dropped, failed};
}
Recorder::~Recorder() { finish(); }
} // namespace recording_session
