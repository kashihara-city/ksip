// A bounded asynchronous WAV recording of a call: the far end on the left,
// this side on the right. Samples come in from the audio threads and go to
// the file from a worker of its own, so the audio never waits on the disk.
// Nothing here knows baresip: the session feeds it plain samples.
#pragma once
#include <array>
#include <condition_variable>
#include <cstdint>
#include <cstdio>
#include <mutex>
#include <string>
#include <thread>

namespace recording_session {
class Recorder {
public:
    // Opens the file (UTF-8 path) and starts the writer; `opened()` says
    // whether the file could be made.
    Recorder(const std::string &path, uint32_t rate);
    ~Recorder();
    Recorder(const Recorder &) = delete;
    Recorder &operator=(const Recorder &) = delete;
    bool opened() const { return file != nullptr; }
    // What the far end sent: it paces the file, and what does not fit in the
    // buffer is dropped and counted.
    void push_far(const int16_t *samples, size_t frames);
    // What this side sent, after the echo canceller and the microphone gain.
    // It is taken as far as it has come, with zeros filling in, thinned when
    // it runs ahead, and never fails the recording.
    void push_near(const int16_t *samples, size_t frames);
    struct Summary {
        uint64_t bytes = 0, dropped = 0;
        bool failed = false;
    };
    // Ends the recording: the writer is joined and the file closed. Complete
    // when nothing failed and nothing was dropped. Safe to call twice.
    Summary finish();
    bool complete() const { return !failed && !dropped; }

private:
    void header();
    void run();
    std::mutex mutex;
    std::condition_variable wake;
    // Left: what the far end sent. Right: what this side sent it. The two
    // come from different threads; the near side is thinned when it runs
    // more than this far ahead.
    static constexpr size_t kNearSlack = 48000 / 5;
    // ("far" and "near" are macros in the Windows headers.)
    std::array<int16_t, 48000 * 10> remote{}, local{};
    size_t far_rd = 0, far_wr = 0, far_count = 0, near_rd = 0, near_wr = 0, near_count = 0;
    uint32_t rate = 0;
    static constexpr uint32_t channels = 2;
    uint64_t bytes = 0, dropped = 0;
    bool done = false, failed = false, finished = false;
    FILE *file = nullptr;
    std::thread worker;
};
} // namespace recording_session
