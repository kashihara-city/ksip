// A bounded asynchronous WAV recording of a call: the far end on the left,
// this side on the right. Samples come in from the audio threads and go to
// the file from a worker of its own, so the audio never waits on the disk.
// Nothing here knows baresip: the session feeds it plain samples.
//
// Time: the file has one rate, chosen when it opens; what comes in at
// another rate (a decoder remade for a codec change, an encoder at a
// different rate than the decoder) is brought to it by linear
// interpolation. Both sides are placed on one clock, the one that started
// with the file: a side whose samples fall short of the time that has
// passed by more than the slack (a far end that stopped sending, this side
// before the microphone opened) has the missing stretch put into its
// stream as silence, in its place: after the samples already in hand and
// before the ones that have just come, so what was heard before the gap
// keeps its place and what comes after it lands where the clock has got
// to. The two channels thus keep their places against each other to within
// the slack, and a side that never comes back is written as silence against
// the other. The clock is wall time at arrival, not the packets' own stamps:
// a side that is late by a whole slack is placed late by that much.
//
// Threads: push_far and push_near are each called from one audio thread;
// the constructor, finish and the destructor from the thread that owns the
// recording. The constructor opens the file and starts the writer, so it is
// not for an audio thread; the session prepares a Recorder before it is put
// in the audio's way.
#pragma once
#include <array>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstdio>
#include <mutex>
#include <string>
#include <thread>

namespace recording_session {
// The highest rate a file is written at and the seconds a side's ring holds;
// the samples a block (one push, one write) is at most.
constexpr uint32_t MAX_RATE = 48000;
constexpr size_t RING_SECONDS = 10, BLOCK_SAMPLES = 8192;
class Recorder {
public:
    // Opens the file (UTF-8 path) at `rate` and starts the writer; `opened()`
    // says whether the file could be made.
    Recorder(const std::string &path, uint32_t rate);
    ~Recorder();
    Recorder(const Recorder &) = delete;
    Recorder &operator=(const Recorder &) = delete;
    [[nodiscard]] bool opened() const { return writing; }
    [[nodiscard]] uint32_t file_rate() const { return rate; }
    // The path the file was opened under: the recording's name to the app.
    [[nodiscard]] const std::string &path() const { return file_path; }
    // What the far end sent: `frames` samples at `rate`, brought to the
    // file's rate. What does not fit in the buffer is dropped and counted.
    void push_far(const int16_t *samples, size_t frames, uint32_t rate);
    // What this side sent, after the echo canceller and the microphone gain,
    // likewise. Kept even while the far end is silent; an overflow is counted.
    void push_near(const int16_t *samples, size_t frames, uint32_t rate);
    struct Summary {
        uint64_t bytes = 0, dropped = 0;
        bool failed = false;
    };
    // Ends the recording: what both sides still hold is written, the writer
    // is joined and the file closed. Complete when nothing failed and
    // nothing was dropped. Safe to call twice.
    Summary finish();
    [[nodiscard]] bool complete() const { return !failed && (dropped == 0u); }

private:
    // One direction: its ring of samples at the file's rate, and the state
    // of the resampler that fills it (touched only by the pushing thread).
    struct Side {
        // Samples at the file's rate, gap silence included, in stream order.
        std::array<int16_t, size_t{MAX_RATE} * RING_SECONDS> ring{};
        size_t rd = 0, wr = 0, count = 0;
        // The input rate seen last, the place of the next output sample
        // between the input samples (index -1 is `last`, the last sample of
        // the chunk before), and that sample.
        uint32_t rate = 0;
        double pos = 0;
        int16_t last = 0;
        // Resampled output on its way to the ring, so that a push allocates nothing.
        std::array<int16_t, BLOCK_SAMPLES> scratch{};
    };
    void push(Side &side, const int16_t *samples, size_t frames, uint32_t rate);
    void append(Side &side, const int16_t *samples, size_t frames);
    // Frames the clock has counted since the file opened, at the file's rate.
    [[nodiscard]] uint64_t clock_frames() const;
    // The next frame of a side; the caller has seen to it that there is one.
    int16_t take(Side &side);
    // Something can go to the file: both sides have samples, or one side has
    // waited longer than the slack for the other.
    [[nodiscard]] bool writable() const;
    bool header();
    void run();
    std::mutex mutex;
    std::condition_variable wake;
    // ("far" and "near" are macros in the Windows headers.)
    Side remote, local;
    uint32_t rate = 0;
    std::string file_path;
    std::chrono::steady_clock::time_point started;
    // Frames written to the file so far, both channels alike.
    uint64_t written = 0;
    // How long one side waits for the other before it goes to the file with
    // silence on the other channel: the arrival jitter of the two sides is
    // well within it, a real gap well beyond it.
    size_t slack = 0;
    static constexpr uint32_t channels = 2;
    uint64_t bytes = 0, dropped = 0;
    bool done = false, failed = false, finished = false;
    // The file opened and the writer runs: set once, before any push, so that
    // the pushing threads may read it without the lock.
    bool writing = false;
    FILE *file = nullptr;
    std::thread worker;
};
} // namespace recording_session
