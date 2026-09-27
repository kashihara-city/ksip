// The WAV recording; see recorder.h.
#include "recorder.h"
#include <algorithm>
#include <cmath>
#include <filesystem>

namespace recording_session {
namespace {
void le16(FILE *f, uint16_t n) {
    fputc(n & 255, f);
    fputc(n >> 8, f);
}
void le32(FILE *f, uint32_t n) {
    le16(f, n & 65535);
    le16(f, n >> 16);
}
} // namespace

// The RIFF/WAVE header, written once the file opens and again with the
// final sizes when it closes: 16-bit PCM, two channels.
void Recorder::header() {
    fseek(file, 0, SEEK_SET);
    fwrite("RIFF", 1, 4, file);
    le32(file, 36 + (uint32_t)bytes);
    fwrite("WAVEfmt ", 1, 8, file);
    le32(file, 16);
    le16(file, 1);
    le16(file, (uint16_t)channels);
    le32(file, rate);
    le32(file, rate * channels * 2);
    le16(file, (uint16_t)(channels * 2));
    le16(file, 16);
    fwrite("data", 1, 4, file);
    le32(file, (uint32_t)bytes);
}
Recorder::Recorder(const std::string &path, uint32_t sr) : rate(sr), slack(sr / 5) {
    file = _wfopen(std::filesystem::u8path(path).c_str(), L"wb");
    if (!file) return;
    writing = true;
    header();
    worker = std::thread([this] { run(); });
}
bool Recorder::writable() const {
    return (remote.count && local.count) || remote.count > slack || local.count > slack;
}
void Recorder::run() {
    std::array<int16_t, 8192> buf; // 4096 frames of two channels
    for (;;) {
        size_t n;
        {
            std::unique_lock<std::mutex> lock(mutex);
            wake.wait(lock, [this] { return done || writable(); });
            if (!remote.count && !local.count && done) break;
            // The two sides pair up as far as both have come. A side alone
            // waits the slack for the other, then goes with silence on the
            // other channel; at the end, whatever is left goes that way.
            bool take_far, take_near;
            if (remote.count && local.count) {
                n = std::min(remote.count, local.count);
                take_far = take_near = true;
            } else if (remote.count) {
                n = done ? remote.count : remote.count > slack ? remote.count - slack : 0;
                take_far = true;
                take_near = false;
            } else {
                n = done ? local.count : local.count > slack ? local.count - slack : 0;
                take_far = false;
                take_near = true;
            }
            if (!n) continue;
            n = std::min(n, buf.size() / 2);
            for (size_t i = 0; i < n; ++i) {
                if (take_far) {
                    buf[2 * i] = remote.ring[remote.rd];
                    remote.rd = (remote.rd + 1) % remote.ring.size();
                } else buf[2 * i] = 0;
                if (take_near) {
                    buf[2 * i + 1] = local.ring[local.rd];
                    local.rd = (local.rd + 1) % local.ring.size();
                } else buf[2 * i + 1] = 0;
            }
            if (take_far) remote.count -= n;
            if (take_near) local.count -= n;
        }
        if (bytes + n * 4 > 0xffff0000ULL || fwrite(buf.data(), 2, 2 * n, file) != 2 * n) {
            failed = true;
            break;
        }
        bytes += n * 4;
    }
    header();
    if (ferror(file) || fflush(file) != 0) failed = true;
}
// Into the side's ring, under the lock; what does not fit is counted.
void Recorder::append(Side &side, const int16_t *samples, size_t frames) {
    std::lock_guard<std::mutex> lock(mutex);
    if (done) return;
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
    const double step = double(in_rate) / rate, end = double(frames) - 1.0;
    for (;;) {
        size_t n = 0;
        while (n < side.scratch.size() && side.pos < end) {
            long idx = (long)std::floor(side.pos);
            double frac = side.pos - idx;
            int a = idx < 0 ? side.last : in[idx], b = in[idx + 1];
            side.scratch[n++] = (int16_t)std::lrint(a + (b - a) * frac);
            side.pos += step;
        }
        if (n) append(side, side.scratch.data(), n);
        if (side.pos >= end) break;
    }
    side.pos -= double(frames);
    side.last = in[frames - 1];
}
void Recorder::push_far(const int16_t *samples, size_t frames, uint32_t in_rate) { push(remote, samples, frames, in_rate); }
void Recorder::push_near(const int16_t *samples, size_t frames, uint32_t in_rate) { push(local, samples, frames, in_rate); }
Recorder::Summary Recorder::finish() {
    if (!finished) {
        finished = true;
        {
            std::lock_guard<std::mutex> lock(mutex);
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
