// A bounded asynchronous WAV recording of a call: the far end on the left,
// this side on the right. Samples come in from the audio threads and go to
// the file from a worker of its own, so the audio never waits on the disk.
// Nothing here knows baresip: the session feeds it plain samples.
//
// Time: the file has one rate, chosen when it opens; what comes in at
// another rate (a decoder remade for a codec change, an encoder at a
// different rate than the decoder) is brought to it by linear
// interpolation, so both channels stay on one time axis. The two sides pair
// up frame by frame as they arrive; a side that goes quiet (a far end that
// stops sending, this side before the microphone opens) is filled with
// silence on its channel, so that neither is lost while the other continues.
//
// Threads: push_far and push_near are each called from one audio thread;
// the constructor, finish and the destructor from the thread that owns the
// recording. The constructor opens the file and starts the writer, so it is
// not for an audio thread; the session prepares a Recorder before it is put
// in the audio's way.
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
    // Opens the file (UTF-8 path) at `rate` and starts the writer; `opened()`
    // says whether the file could be made.
    Recorder(const std::string &path, uint32_t rate);
    ~Recorder();
    Recorder(const Recorder &) = delete;
    Recorder &operator=(const Recorder &) = delete;
    bool opened() const { return writing; }
    uint32_t file_rate() const { return rate; }
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
    bool complete() const { return !failed && !dropped; }

private:
    // One direction: its ring of samples at the file's rate, and the state
    // of the resampler that fills it (touched only by the pushing thread).
    struct Side {
        std::array<int16_t, 48000 * 10> ring{};
        size_t rd = 0, wr = 0, count = 0;
        // The input rate seen last, the place of the next output sample
        // between the input samples (index -1 is `last`, the last sample of
        // the chunk before), and that sample.
        uint32_t rate = 0;
        double pos = 0;
        int16_t last = 0;
        // Resampled output on its way to the ring, so that a push allocates nothing.
        std::array<int16_t, 8192> scratch{};
    };
    void push(Side &side, const int16_t *samples, size_t frames, uint32_t rate);
    void append(Side &side, const int16_t *samples, size_t frames);
    // Something can go to the file: both sides have samples, or one side has
    // waited longer than the slack for the other.
    bool writable() const;
    void header();
    void run();
    std::mutex mutex;
    std::condition_variable wake;
    // ("far" and "near" are macros in the Windows headers.)
    Side remote, local;
    uint32_t rate = 0;
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
