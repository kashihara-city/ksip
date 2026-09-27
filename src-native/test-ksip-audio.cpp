// Unit tests of the audio module's parts that need no device, no baresip
// and no WebRTC: who gets the playout stream and when it is handed back
// (also after a start that fails), what a source does without a microphone,
// the callback slot that is cleared while a call runs, and the WAV recorder.
// Built and run by scripts/test/audio-module.ps1.
#include "ksip_audio/recorder.h"
#include "ksip_audio/session_core.h"
#include "webrtc/callback_gate.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <memory>
#include <string>
#include <thread>
#include <vector>

namespace {
int failures = 0;
void check(bool ok, const char *what) {
    std::printf("%s: %s\n", ok ? "PASS" : "FAIL", what);
    if (!ok) ++failures;
}

// ---- the session core, against a bridge that only keeps a diary
struct Player {
    std::string name;
    bool started = false;
    const char *device() const { return name.c_str(); }
};
struct Source {
    bool started = false;
};
struct FakeAdm : playback_session::Adm {
    std::vector<std::string> diary;
    void *playing = nullptr, *recording = nullptr;
    bool playout_on = false, recording_on = false;
    // Devices whose start fails, by name.
    std::vector<std::string> broken;
    bool fails(const char *device) {
        for (auto &b : broken) if (b == device) return true;
        return false;
    }
    int start_playout(const char *device, void *player) override {
        diary.push_back(std::string("start_playout ") + device);
        if (fails(device)) return -3;
        playing = player;
        playout_on = true;
        return 0;
    }
    void detach_playout() override {
        diary.push_back("detach_playout");
        playing = nullptr;
    }
    bool playout_running() override { return playout_on; }
    void stop_playout() override {
        diary.push_back("stop_playout");
        playout_on = false;
        playing = nullptr;
    }
    int start_recording(const char *device, void *source) override {
        diary.push_back(std::string("start_recording ") + device);
        if (fails(device)) return -3;
        recording = source;
        recording_on = true;
        return 0;
    }
    void detach_recording() override {
        diary.push_back("detach_recording");
        recording = nullptr;
    }
    bool recording_running() override { return recording_on; }
    void stop_recording() override {
        diary.push_back("stop_recording");
        recording_on = false;
        recording = nullptr;
    }
};
struct FakeClock : playback_session::Clock {
    std::vector<std::string> diary;
    bool linger = false, handback = false;
    void start(playback_session::Timer which, uint64_t ms) override {
        (which == playback_session::Timer::Linger ? linger : handback) = true;
        diary.push_back(std::string(which == playback_session::Timer::Linger ? "linger " : "handback ") + std::to_string(ms));
    }
    void cancel(playback_session::Timer which) override { (which == playback_session::Timer::Linger ? linger : handback) = false; }
};
struct Fixture {
    FakeAdm adm;
    FakeClock clock;
    std::vector<std::string> log;
    std::vector<Source *> fallbacks;
    playback_session::Core<Player, Source> core;
    Fixture()
        : core(adm, clock,
               {[this](const char *line) { log.emplace_back(line); },
                [this](bool, const char *device, int result) { log.push_back(std::string("failed ") + device + " " + std::to_string(result)); },
                [this](Source *s) { fallbacks.push_back(s); },
                [this](Source *s) { fallbacks.erase(std::remove(fallbacks.begin(), fallbacks.end(), s), fallbacks.end()); }}) {}
    bool logged(const char *part) const {
        for (auto &l : log) if (l.find(part) != std::string::npos) return true;
        return false;
    }
};

void the_newest_player_has_the_stream_and_the_one_before_gets_it_back() {
    Fixture f;
    Player tone{"speaker-a"}, call{"speaker-a"};
    check(f.core.take_playout(&tone) == 0 && f.adm.playing == &tone, "the first player gets the stream");
    check(f.logged("playout started"), "a fresh stream is said to have started");
    check(f.core.take_playout(&call) == 0 && f.adm.playing == &call && !tone.started, "the second player takes the stream over");
    check(f.logged("playout taken over"), "the takeover is said so");
    f.core.playout_gone(&call);
    check(f.adm.playing == nullptr && f.clock.handback, "the stream is detached and the hand-back is scheduled, not done at once");
    f.core.hand_back();
    check(f.adm.playing == &tone && tone.started, "the player before gets the stream back");
    check(f.logged("playout handed back\n"), "the hand-back is said so");
    f.core.playout_gone(&tone);
    f.core.hand_back();
    check(f.clock.linger && f.adm.playing == nullptr, "with nobody left the stream idles and is closed after a while");
    f.core.stop_idle();
    check(!f.adm.playout_on, "the idle stream is closed");
}
void a_start_that_fails_gives_the_stream_back_to_the_player_it_took_it_from() {
    Fixture f;
    Player tone{"speaker-a"}, call{"speaker-broken"};
    f.core.take_playout(&tone);
    f.adm.broken.push_back("speaker-broken");
    f.adm.diary.clear();
    check(f.core.take_playout(&call) == ENODEV, "the failed start is refused");
    check(f.adm.diary.size() == 3 && f.adm.diary[0] == "detach_playout" && f.adm.diary[1] == "start_playout speaker-broken" && f.adm.diary[2] == "start_playout speaker-a",
          "the old player is detached, the new start tried, the old start restored, in that order");
    check(f.adm.playing == &tone && tone.started && f.core.playout() == &tone, "the old player has its stream back");
    check(f.logged("failed speaker-broken -3") && f.logged("handed back after a failed start"), "the failure and the hand-back are on record");
    check(!f.clock.linger, "nothing idles while the old player plays");
    check(f.core.player_count() == 1, "the failed player is not kept");
}
void a_start_that_fails_with_nobody_to_hand_back_to_lets_the_streams_idle() {
    Fixture f;
    Player call{"speaker-broken"};
    f.adm.broken.push_back("speaker-broken");
    check(f.core.take_playout(&call) == ENODEV && f.core.playout() == nullptr, "the start fails and nobody has the stream");
    check(f.clock.linger, "the idle streams are left to close in their own time");
}
void a_restore_that_fails_too_lets_the_streams_idle() {
    Fixture f;
    Player tone{"speaker-a"}, call{"speaker-broken"};
    f.core.take_playout(&tone);
    f.adm.broken = {"speaker-broken", "speaker-a"};
    check(f.core.take_playout(&call) == ENODEV && f.core.playout() == nullptr && !tone.started, "neither the new nor the old start works");
    check(f.clock.linger, "the streams idle");
}
void a_microphone_that_will_not_open_is_replaced_by_silence_until_the_source_goes() {
    Fixture f;
    Source mic;
    f.adm.broken.push_back("mic-broken");
    check(f.core.take_source(&mic, "mic-broken") == 0, "the call goes on without a microphone");
    check(f.fallbacks.size() == 1 && f.fallbacks[0] == &mic && !mic.started, "the fallback runs for the source");
    check(f.logged("failed mic-broken -3"), "the failure is on record");
    Source next;
    check(f.core.take_source(&next, "mic-ok") == 0 && next.started && f.adm.recording == &next, "a new source with a working microphone takes over");
    check(f.fallbacks.empty(), "the fallback of the replaced source is stopped");
    f.core.source_gone(&next);
    check(f.adm.recording == nullptr && f.clock.linger, "the microphone is detached and idles");
}
void the_microphone_is_opened_ahead_only_once_and_only_when_free() {
    Fixture f;
    f.core.open_microphone_ahead("mic-ok");
    check(f.adm.recording_on && f.adm.recording == nullptr, "the microphone runs into nothing ahead of the call");
    f.adm.diary.clear();
    f.core.open_microphone_ahead("mic-ok");
    check(f.adm.diary.empty(), "a microphone already open is left alone");
    Source mic;
    f.core.take_source(&mic, "mic-ok");
    check(f.adm.recording == &mic && f.logged("recording taken over, APM running"), "the source takes the stream that was opened ahead");
}

// ---- the callback gate
using Callback = int (*)(void *, int);
std::atomic<int> callback_visits{0};
std::atomic<bool> hold_callback{true};
int slow_callback(void *arg, int) {
    ++callback_visits;
    while (hold_callback.load()) std::this_thread::sleep_for(std::chrono::milliseconds(1));
    // The argument is still valid here: the clear waits for this to return.
    return *static_cast<int *>(arg);
}
void clearing_a_callback_waits_for_the_call_in_flight() {
    ksip_audio_internal::CallbackGate<Callback> gate;
    int payload = 7;
    gate.Set(slow_callback, &payload);
    std::atomic<bool> invoked{false}, returned{false};
    std::thread audio([&] {
        gate.Invoke([&](Callback cb, void *arg) {
            invoked = true;
            if (cb) cb(arg, 0);
        });
        returned = true;
    });
    while (!invoked) std::this_thread::sleep_for(std::chrono::milliseconds(1));
    std::atomic<bool> cleared{false};
    std::thread clearer([&] {
        gate.ClearAndDrain();
        cleared = true;
    });
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
    check(!cleared, "the clear waits while the callback runs");
    hold_callback = false;
    clearer.join();
    audio.join();
    check(cleared && returned, "the clear returns once the callback has returned");
    bool ran = false;
    gate.Invoke([&](Callback cb, void *) { ran = cb != nullptr; });
    check(!ran, "after the clear nothing is called");
}
int clearing_callback(void *arg, int) {
    // Clearing from inside the callback must not wait on itself.
    static_cast<ksip_audio_internal::CallbackGate<Callback> *>(arg)->ClearAndDrain();
    return 0;
}
void clearing_from_inside_the_callback_does_not_wait_on_itself() {
    ksip_audio_internal::CallbackGate<Callback> gate;
    gate.Set(clearing_callback, &gate);
    std::atomic<bool> done{false};
    std::thread audio([&] {
        gate.Invoke([&](Callback cb, void *arg) {
            if (cb) cb(arg, 0);
        });
        done = true;
    });
    auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(2);
    while (!done && std::chrono::steady_clock::now() < deadline) std::this_thread::sleep_for(std::chrono::milliseconds(1));
    check(done, "the callback that clears its own slot returns");
    if (done) audio.join();
    else audio.detach();
    check(!gate.Installed(), "and the slot is empty afterwards");
}
void a_late_set_after_the_clear_is_seen_by_the_next_call() {
    ksip_audio_internal::CallbackGate<Callback> gate;
    int a = 1, b = 2;
    gate.Set(slow_callback, &a);
    gate.ClearAndDrain();
    gate.Set(slow_callback, &b);
    void *seen = nullptr;
    gate.Invoke([&](Callback, void *arg) { seen = arg; });
    check(seen == &b, "the slot holds what was set last");
}

// ---- the recorder
uint32_t le32(const std::vector<unsigned char> &d, size_t at) { return d[at] | d[at + 1] << 8 | d[at + 2] << 16 | (uint32_t)d[at + 3] << 24; }
std::vector<unsigned char> read_file(const std::filesystem::path &p) {
    std::ifstream in(p, std::ios::binary);
    return std::vector<unsigned char>((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
}
void a_recording_writes_both_sides_with_sizes_in_the_header() {
    auto dir = std::filesystem::temp_directory_path() / "ksip-recorder-test";
    std::filesystem::create_directories(dir);
    auto path = dir / "both.wav";
    {
        auto rp = std::make_unique<recording_session::Recorder>(path.string(), 8000);
    auto &r = *rp;
        check(r.opened(), "the file opens");
        std::vector<int16_t> far(800, 1000), near(400, -500);
        r.push_near(near.data(), near.size());
        r.push_far(far.data(), far.size());
        auto summary = r.finish();
        check(!summary.failed && summary.dropped == 0 && summary.bytes == 800 * 4, "every far frame is written, four bytes each");
    }
    auto bytes = read_file(path);
    check(bytes.size() == 44 + 800 * 4, "the file is the header and the data");
    check(std::memcmp(bytes.data(), "RIFF", 4) == 0 && le32(bytes, 4) == 36 + 800 * 4 && le32(bytes, 40) == 800 * 4, "the header carries the final sizes");
    check(le32(bytes, 24) == 8000 && bytes[22] == 2, "the rate and the two channels are in the header");
    int16_t first_left, first_right, late_right;
    std::memcpy(&first_left, &bytes[44], 2);
    std::memcpy(&first_right, &bytes[46], 2);
    std::memcpy(&late_right, &bytes[44 + 700 * 4 + 2], 2);
    check(first_left == 1000 && first_right == -500, "left is the far end, right is this side");
    check(late_right == 0, "where this side has nothing, zeros fill in");
    std::filesystem::remove_all(dir);
}
void far_frames_beyond_the_buffer_are_dropped_and_counted() {
    auto dir = std::filesystem::temp_directory_path() / "ksip-recorder-test-drop";
    std::filesystem::create_directories(dir);
    auto rp = std::make_unique<recording_session::Recorder>((dir / "drop.wav").string(), 48000);
    auto &r = *rp;
    // The writer only drains once woken; a burst beyond ten seconds of audio
    // in one push cannot all fit.
    std::vector<int16_t> burst(48000 * 10 + 480, 1);
    r.push_far(burst.data(), burst.size());
    auto summary = r.finish();
    check(summary.dropped == 480, "what did not fit is counted");
    check(!r.complete(), "a recording with dropped samples is not complete");
    std::filesystem::remove_all(dir);
}
void a_file_that_cannot_be_opened_is_reported_not_written() {
    auto rp = std::make_unique<recording_session::Recorder>("Z:/no/such/folder/x.wav", 48000);
    auto &r = *rp;
    check(!r.opened(), "the recorder says the file did not open");
    std::vector<int16_t> far(480, 1);
    r.push_far(far.data(), far.size());
    auto summary = r.finish();
    check(summary.bytes == 0, "nothing is written");
}
void finishing_twice_is_once() {
    auto dir = std::filesystem::temp_directory_path() / "ksip-recorder-test-twice";
    std::filesystem::create_directories(dir);
    auto rp = std::make_unique<recording_session::Recorder>((dir / "twice.wav").string(), 48000);
    auto &r = *rp;
    std::vector<int16_t> far(480, 1);
    r.push_far(far.data(), far.size());
    auto first = r.finish(), second = r.finish();
    check(first.bytes == second.bytes && !second.failed, "the second finish reports the same and fails nothing");
    std::filesystem::remove_all(dir);
}
} // namespace

int main() {
    the_newest_player_has_the_stream_and_the_one_before_gets_it_back();
    a_start_that_fails_gives_the_stream_back_to_the_player_it_took_it_from();
    a_start_that_fails_with_nobody_to_hand_back_to_lets_the_streams_idle();
    a_restore_that_fails_too_lets_the_streams_idle();
    a_microphone_that_will_not_open_is_replaced_by_silence_until_the_source_goes();
    the_microphone_is_opened_ahead_only_once_and_only_when_free();
    clearing_a_callback_waits_for_the_call_in_flight();
    clearing_from_inside_the_callback_does_not_wait_on_itself();
    a_late_set_after_the_clear_is_seen_by_the_next_call();
    a_recording_writes_both_sides_with_sizes_in_the_header();
    far_frames_beyond_the_buffer_are_dropped_and_counted();
    a_file_that_cannot_be_opened_is_reported_not_written();
    finishing_twice_is_once();
    std::printf("%s: %d failure(s)\n", failures ? "FAIL" : "PASS", failures);
    return failures ? 1 : 0;
}
