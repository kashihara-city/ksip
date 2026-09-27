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
Recorder::Recorder(const std::string &path, uint32_t sr) : rate(sr), started(std::chrono::steady_clock::now()), slack(sr / 5) {
    file = _wfopen(std::filesystem::u8path(path).c_str(), L"wb");
    if (!file) return;
    writing = true;
    header();
    worker = std::thread([this] { run(); });
}
bool Recorder::writable() const {
    return (remote.available() && local.available()) || remote.available() > slack || local.available() > slack;
}
uint64_t Recorder::clock_frames() const {
    auto passed = std::chrono::duration_cast<std::chrono::microseconds>(std::chrono::steady_clock::now() - started).count();
    return uint64_t(passed) * rate / 1000000;
}
int16_t Recorder::take(Side &side) {
    if (side.silence) {
        --side.silence;
        return 0;
    }
    int16_t v = side.ring[side.rd];
    side.rd = (side.rd + 1) % side.ring.size();
    --side.count;
    return v;
}
void Recorder::run() {
    std::array<int16_t, 8192> buf; // 4096 frames of two channels
    for (;;) {
        size_t n;
        {
            std::unique_lock<std::mutex> lock(mutex);
            wake.wait(lock, [this] { return done || writable(); });
            if (done) {
                // Silence owed at the very end, with no samples behind it on
                // either side, would only lengthen the file: it is let go.
                if (!remote.count) remote.silence = 0;
                if (!local.count) local.silence = 0;
                if (!remote.available() && !local.available()) break;
            }
            // The two sides pair up as far as both have come, each giving
            // the silence it owes before its samples. A side alone waits the
            // slack for the other, then goes with silence on the other
            // channel; at the end, whatever is left goes that way.
            bool take_far, take_near;
            const size_t far_have = remote.available(), near_have = local.available();
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
                buf[2 * i] = take_far ? take(remote) : int16_t(0);
                buf[2 * i + 1] = take_near ? take(local) : int16_t(0);
            }
            written += n;
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
// Into the side's ring, under the lock; what does not fit is counted. First
// the side is placed on the clock: the frames it has accounted for (written,
// owed as silence or in hand) are compared with the frames the clock has
// counted, and a shortfall beyond the slack is owed as silence in front of
// these samples. Arrival jitter stays within the slack and moves nothing; a
// real gap is filled to the clock, so that the side comes back where the
// other side is by then. Samples that were dropped for want of room are
// made up the same way, as silence.
void Recorder::append(Side &side, const int16_t *samples, size_t frames) {
    std::lock_guard<std::mutex> lock(mutex);
    if (done) return;
    const uint64_t accounted = written + side.available(), now = clock_frames();
    if (now > accounted + slack) side.silence += size_t(now - accounted);
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
