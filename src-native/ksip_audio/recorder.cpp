// The WAV recording; see recorder.h.
#include "recorder.h"
#include <algorithm>
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
Recorder::Recorder(const std::string &path, uint32_t sr) : rate(sr) {
    file = _wfopen(std::filesystem::u8path(path).c_str(), L"wb");
    if (!file) return;
    header();
    worker = std::thread([this] { run(); });
}
void Recorder::run() {
    std::array<int16_t, 8192> buf; // 4096 frames of two channels
    for (;;) {
        size_t n;
        {
            std::unique_lock<std::mutex> lock(mutex);
            wake.wait(lock, [this] { return done || far_count; });
            if (!far_count && done) break;
            n = std::min(far_count, buf.size() / 2);
            if (near_count > n + kNearSlack) {
                size_t excess = near_count - n - kNearSlack;
                near_rd = (near_rd + excess) % local.size();
                near_count -= excess;
            }
            for (size_t i = 0; i < n; ++i) {
                buf[2 * i] = remote[far_rd];
                far_rd = (far_rd + 1) % remote.size();
                if (near_count) {
                    buf[2 * i + 1] = local[near_rd];
                    near_rd = (near_rd + 1) % local.size();
                    --near_count;
                } else buf[2 * i + 1] = 0;
            }
            far_count -= n;
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
void Recorder::push_far(const int16_t *samples, size_t frames) {
    std::lock_guard<std::mutex> lock(mutex);
    if (!file || done) return;
    for (size_t i = 0; i < frames; ++i) {
        if (far_count == remote.size()) {
            dropped += frames - i;
            break;
        }
        remote[far_wr] = samples[i];
        far_wr = (far_wr + 1) % remote.size();
        ++far_count;
    }
    wake.notify_one();
}
void Recorder::push_near(const int16_t *samples, size_t frames) {
    std::lock_guard<std::mutex> lock(mutex);
    if (!file || done) return;
    // The near side never fails the recording: what does not fit is left out.
    for (size_t i = 0; i < frames && near_count < local.size(); ++i) {
        local[near_wr] = samples[i];
        near_wr = (near_wr + 1) % local.size();
        ++near_count;
    }
}
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
