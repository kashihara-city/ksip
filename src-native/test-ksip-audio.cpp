// Unit tests of the audio module's parts that need no device, no baresip
// and no WebRTC: who gets the playout stream and when it is handed back
// (also after a start that fails), what a source does without a microphone,
// the callback slot that is cleared while a call runs, and the WAV recorder.
// Built and run by scripts/test/audio-module.ps1.
#include "ksip_audio/recorder.h"
#include "ksip_audio/session_core.h"
#include "ksip/ksip_text.h"
#include "webrtc/callback_gate.h"

#include <atomic>
#include <cctype>
#include <chrono>
#include <cmath>
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
// A source goes as the module lets it go: its fallback first, then the core.
void StopAll(Fixture &f, Source &source) {
    f.fallbacks.erase(std::remove(f.fallbacks.begin(), f.fallbacks.end(), &source), f.fallbacks.end());
    f.core.source_gone(&source);
}
void the_state_follows_the_microphone_from_silence_to_the_device_and_back() {
    using playback_session::Input;
    Fixture f;
    check(f.core.input() == Input::None && f.core.microphone_failures().count == 0, "before any call there is no input and no failure");
    Source first, second, third, fourth;
    f.adm.broken.push_back("mic-broken");
    f.core.take_source(&first, "mic-broken");
    check(f.core.input() == Input::Silence && f.core.microphone_failures().count == 1, "a microphone that would not open: the input is silence, one failure");
    check(f.core.microphone_failures().last_result == -3 && f.core.speaker_failures().count == 0, "the failure is the microphone's, with the bridge's result");
    f.core.take_source(&second, "mic-broken");
    check(f.core.input() == Input::Silence && f.core.microphone_failures().count == 2, "silence replaced by silence: still silence, the failure counted again");
    f.core.take_source(&third, "mic-ok");
    check(f.core.input() == Input::Device && f.core.microphone_failures().count == 2, "a later start that works: the input is the device, and the failures stay counted");
    f.adm.broken.push_back("mic-late");
    f.core.take_source(&fourth, "mic-late");
    check(f.core.input() == Input::Silence && f.core.microphone_failures().count == 3, "the device replaced by one that fails: silence again");
    StopAll(f, fourth);
    check(f.core.input() == Input::None, "the source gone: no input, whatever it was");
}
void the_state_says_the_speaker_plays_after_a_failed_start_handed_back() {
    Fixture f;
    Player tone{"speaker-a"}, call{"speaker-broken"}, later{"speaker-a"};
    f.core.take_playout(&tone);
    f.adm.broken.push_back("speaker-broken");
    f.core.take_playout(&call);
    check(f.core.playout() == &tone && f.core.speaker_failures().count == 1 && f.core.speaker_failures().last_result == -3,
          "the failed start is counted as the speaker's, and the tone plays on");
    f.core.take_playout(&later);
    check(f.core.playout() == &later && f.core.speaker_failures().count == 1, "a later start that works: the speaker plays, the failure stays counted");
    f.core.playout_gone(&later);
    f.core.hand_back();
    f.core.playout_gone(&tone);
    f.core.hand_back();
    check(f.core.playout() == nullptr && f.core.speaker_failures().count == 1, "every player gone: nothing plays, and nothing failed in going");
}
void a_failure_of_the_speaker_is_not_hidden_by_a_later_one_of_the_microphone() {
    Fixture f;
    Player call{"speaker-broken"};
    Source mic;
    f.adm.broken = {"speaker-broken", "mic-broken"};
    f.core.take_playout(&call);
    f.core.take_source(&mic, "mic-broken");
    check(f.core.speaker_failures().count == 1 && f.core.speaker_failures().last_result == -3, "the speaker's failure stands after the microphone's");
    check(f.core.microphone_failures().count == 1, "and the microphone's is counted on its own");
    StopAll(f, mic);
}
void a_hand_back_that_fails_is_counted_as_the_speakers() {
    Fixture f;
    Player tone{"speaker-a"}, call{"speaker-b"};
    f.core.take_playout(&tone);
    f.core.take_playout(&call);
    f.adm.broken.push_back("speaker-a");
    f.core.playout_gone(&call);
    f.log.clear();
    f.core.hand_back();
    check(f.core.playout() == nullptr && f.clock.linger, "the player left cannot have the stream back, and the streams idle");
    check(f.core.speaker_failures().count == 1 && f.logged("failed speaker-a -3"), "the failed hand-back is counted and logged as the speaker's");
}
void a_restore_that_fails_after_a_failed_start_is_counted_too() {
    Fixture f;
    Player tone{"speaker-a"}, call{"speaker-broken"};
    f.core.take_playout(&tone);
    f.adm.broken = {"speaker-broken", "speaker-a"};
    f.core.take_playout(&call);
    check(f.core.speaker_failures().count == 2 && f.logged("failed speaker-broken -3") && f.logged("failed speaker-a -3"),
          "the new start and the restore of the old one are both counted and logged");
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
        r.push_near(near.data(), near.size(), 8000);
        r.push_far(far.data(), far.size(), 8000);
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
    r.push_far(burst.data(), burst.size(), 48000);
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
    r.push_far(far.data(), far.size(), 48000);
    auto summary = r.finish();
    check(summary.bytes == 0, "nothing is written");
}
void finishing_twice_is_once() {
    auto dir = std::filesystem::temp_directory_path() / "ksip-recorder-test-twice";
    std::filesystem::create_directories(dir);
    auto rp = std::make_unique<recording_session::Recorder>((dir / "twice.wav").string(), 48000);
    auto &r = *rp;
    std::vector<int16_t> far(480, 1);
    r.push_far(far.data(), far.size(), 48000);
    auto first = r.finish(), second = r.finish();
    check(first.bytes == second.bytes && !second.failed, "the second finish reports the same and fails nothing");
    std::filesystem::remove_all(dir);
}

// The samples of one channel of a 16-bit stereo WAV file's data.
std::vector<int16_t> channel(const std::vector<unsigned char> &bytes, int which) {
    std::vector<int16_t> out;
    for (size_t at = 44 + 2 * which; at + 1 < bytes.size(); at += 4) {
        int16_t v;
        std::memcpy(&v, &bytes[at], 2);
        out.push_back(v);
    }
    return out;
}
// A sine of `hz` at `rate` for `seconds`, as the codecs hand samples over.
std::vector<int16_t> sine(double hz, uint32_t rate, double seconds, double amplitude = 8000) {
    std::vector<int16_t> out(size_t(rate * seconds));
    for (size_t i = 0; i < out.size(); ++i) out[i] = (int16_t)(amplitude * std::sin(2 * 3.14159265358979 * hz * i / rate));
    return out;
}
// How often the signal crosses zero upwards: twice per period in a sine.
size_t rising_crossings(const std::vector<int16_t> &s) {
    size_t n = 0;
    for (size_t i = 1; i < s.size(); ++i) if (s[i - 1] < 0 && s[i] >= 0) ++n;
    return n;
}
void this_side_is_kept_while_the_far_end_is_silent() {
    auto dir = std::filesystem::temp_directory_path() / "ksip-recorder-test-near-only";
    std::filesystem::create_directories(dir);
    auto path = dir / "near.wav";
    {
        auto rp = std::make_unique<recording_session::Recorder>(path.string(), 8000);
        std::vector<int16_t> near(8000, 700);
        rp->push_near(near.data(), near.size(), 8000);
        auto summary = rp->finish();
        check(!summary.failed && summary.dropped == 0 && summary.bytes == 8000 * 4, "a second of this side alone is a second of file");
    }
    auto bytes = read_file(path);
    auto left = channel(bytes, 0), right = channel(bytes, 1);
    check(left.size() == 8000 && left[4000] == 0 && right[4000] == 700, "the far channel is silence, this side is what was sent");
    std::filesystem::remove_all(dir);
}
void samples_at_another_rate_are_brought_to_the_file_rate() {
    auto dir = std::filesystem::temp_directory_path() / "ksip-recorder-test-rates";
    std::filesystem::create_directories(dir);
    auto path = dir / "rates.wav";
    {
        auto rp = std::make_unique<recording_session::Recorder>(path.string(), 48000);
        // The far end at 8 kHz in 20 ms frames, this side at 16 kHz in 10 ms
        // frames, both one second of a tone, arriving as they do in a call:
        // in step with each other.
        auto far = sine(400, 8000, 1.0), near = sine(1000, 16000, 1.0);
        for (size_t ms = 0; ms < 1000; ms += 20) {
            rp->push_far(far.data() + ms * 8, 160, 8000);
            rp->push_near(near.data() + ms * 16, 160, 16000);
            rp->push_near(near.data() + (ms + 10) * 16, 160, 16000);
        }
        auto summary = rp->finish();
        check(!summary.failed && summary.dropped == 0, "nothing is dropped on the way to the file rate");
    }
    auto bytes = read_file(path);
    check(le32(bytes, 24) == 48000, "the header keeps the file rate");
    auto left = channel(bytes, 0), right = channel(bytes, 1);
    check(left.size() >= 48000 - 16 && left.size() <= 48000 + 16, "a second in is a second out, whatever the input rate");
    size_t far_periods = rising_crossings(left), near_periods = rising_crossings(right);
    check(far_periods >= 398 && far_periods <= 401, "the far tone keeps its pitch at the file rate");
    check(near_periods >= 998 && near_periods <= 1001, "this side's tone keeps its pitch at the file rate");
    std::filesystem::remove_all(dir);
}
void a_gap_in_the_far_end_leaves_this_side_in_place() {
    auto dir = std::filesystem::temp_directory_path() / "ksip-recorder-test-gap";
    std::filesystem::create_directories(dir);
    auto path = dir / "gap.wav";
    auto settle = [] { std::this_thread::sleep_for(std::chrono::milliseconds(150)); };
    {
        auto rp = std::make_unique<recording_session::Recorder>(path.string(), 8000);
        std::vector<int16_t> far1(8000, 1000), near1(8000, -500), near2(8000, -600), far3(8000, 2000), near3(8000, -700);
        // Second one: both sides. Second two: the far end stops, this side
        // goes on. Second three: both again.
        rp->push_far(far1.data(), far1.size(), 8000);
        rp->push_near(near1.data(), near1.size(), 8000);
        settle();
        rp->push_near(near2.data(), near2.size(), 8000);
        settle();
        rp->push_far(far3.data(), far3.size(), 8000);
        rp->push_near(near3.data(), near3.size(), 8000);
        auto summary = rp->finish();
        check(!summary.failed && summary.dropped == 0 && summary.bytes == 24000 * 4, "three seconds of file, nothing dropped");
    }
    auto bytes = read_file(path);
    auto left = channel(bytes, 0), right = channel(bytes, 1);
    check(left.size() == 24000, "the length is the time that passed");
    check(left[4000] == 1000 && right[4000] == -500, "the first second pairs the two sides");
    check(left[12000] == 0 && right[12000] == -600, "in the gap this side is kept, with silence for the far end");
    check(left[20000] == 2000 && right[20000] == -700, "after the gap the sides pair again");
    check(left[23600] == 0 && right[23600] == -700, "what this side still held at the end is written with silence");
    std::filesystem::remove_all(dir);
}
void a_side_that_starts_late_is_placed_where_the_clock_says() {
    auto dir = std::filesystem::temp_directory_path() / "ksip-recorder-test-clock";
    std::filesystem::create_directories(dir);
    auto path = dir / "clock.wav";
    {
        auto rp = std::make_unique<recording_session::Recorder>(path.string(), 8000);
        // This side speaks for 200 ms; the far end says nothing for 230 ms
        // and then both go on. The far end's first sample belongs at about
        // 230 ms into the file, not at its start.
        std::vector<int16_t> near1(1600, -500), far2(1600, 1000), near2(1600, -600);
        rp->push_near(near1.data(), near1.size(), 8000);
        std::this_thread::sleep_for(std::chrono::milliseconds(230));
        rp->push_far(far2.data(), far2.size(), 8000);
        rp->push_near(near2.data(), near2.size(), 8000);
        auto summary = rp->finish();
        check(!summary.failed && summary.dropped == 0, "nothing is lost while a side waits");
    }
    auto bytes = read_file(path);
    auto left = channel(bytes, 0), right = channel(bytes, 1);
    size_t first_far = 0;
    while (first_far < left.size() && left[first_far] == 0) ++first_far;
    check(first_far >= 1600 && first_far <= 2800, "the far end appears where the clock had got to when it started");
    check(right[100] == -500 && left[100] == 0, "this side's first samples are at the start, against silence");
    check(left.size() >= 3200 && left.size() <= 4400, "the file is as long as the time that passed");
    std::filesystem::remove_all(dir);
}
void silence_for_a_gap_goes_after_the_samples_already_in_hand() {
    auto dir = std::filesystem::temp_directory_path() / "ksip-recorder-test-order";
    std::filesystem::create_directories(dir);
    auto path = dir / "order.wav";
    {
        auto rp = std::make_unique<recording_session::Recorder>(path.string(), 8000);
        // 200 ms of the far end (within the slack, so it waits in the ring),
        // then nothing for 550 ms, then more. What came first stays first;
        // the gap's silence sits between the two.
        std::vector<int16_t> first(1600, 1000), second(1600, 2000);
        rp->push_far(first.data(), first.size(), 8000);
        std::this_thread::sleep_for(std::chrono::milliseconds(550));
        rp->push_far(second.data(), second.size(), 8000);
        rp->finish();
    }
    auto left = channel(read_file(path), 0);
    check(left.size() > 1600 && left[0] == 1000 && left[1599] == 1000, "the samples already in hand keep the start of the file");
    size_t second_at = 1600;
    while (second_at < left.size() && left[second_at] != 2000) ++second_at;
    check(second_at < left.size() && left[second_at - 1] == 0 && second_at >= 3600 && second_at <= 5600, "the later samples come after the gap's silence, where the clock had got to");
    std::filesystem::remove_all(dir);
}
void a_rate_change_in_the_middle_keeps_the_time_axis() {
    auto dir = std::filesystem::temp_directory_path() / "ksip-recorder-test-change";
    std::filesystem::create_directories(dir);
    auto path = dir / "change.wav";
    {
        auto rp = std::make_unique<recording_session::Recorder>(path.string(), 8000);
        // A PCMU call whose far end changes to Opus: a second at 8 kHz, then
        // a second at 48 kHz, the same tone.
        auto first = sine(300, 8000, 1.0), second = sine(300, 48000, 1.0);
        for (size_t at = 0; at < first.size(); at += 160) rp->push_far(first.data() + at, 160, 8000);
        for (size_t at = 0; at < second.size(); at += 960) rp->push_far(second.data() + at, 960, 48000);
        rp->finish();
    }
    auto left = channel(read_file(path), 0);
    check(left.size() >= 16000 - 16 && left.size() <= 16000 + 16, "two seconds of file for two seconds of audio");
    size_t periods = rising_crossings(left);
    check(periods >= 598 && periods <= 601, "the tone keeps its pitch across the change");
    std::filesystem::remove_all(dir);
}

// ---- the SIP text
std::vector<std::string> scrubbed(const std::string &text) {
    return ksip_text::scrubbed_sip_lines(reinterpret_cast<const uint8_t *>(text.data()), text.size());
}
bool exposes(const std::vector<std::string> &lines, const char *secret) {
    for (auto &l : lines) if (l.find(secret) != std::string::npos) return true;
    return false;
}
void digest_headers_are_hidden_however_they_are_written() {
    const char *names[] = {"Authorization", "Proxy-Authorization", "WWW-Authenticate", "Proxy-Authenticate"};
    const char *spacings[] = {"", " ", "\t", " \t "};
    for (auto name : names) {
        for (auto spacing : spacings) {
            std::string lower = name;
            for (auto &c : lower) c = (char)std::tolower((unsigned char)c);
            for (const std::string &written : {std::string(name), lower}) {
                std::string text = "REGISTER sip:pbx.example SIP/2.0\r\n" + written + spacing + ": Digest username=\"1001\", response=\"SECRETRESPONSE\",\r\n"
                                   " nonce=\"SECRETNONCE\"\r\n\tqop=auth\r\nCSeq: 2 REGISTER\r\n";
                auto lines = scrubbed(text);
                bool ok = !exposes(lines, "SECRETRESPONSE") && !exposes(lines, "SECRETNONCE") && exposes(lines, "***") && exposes(lines, "CSeq: 2 REGISTER");
                if (!ok) std::printf("  %s with spacing [%s]\n", written.c_str(), spacing);
                check(ok, "a digest header and its folded lines are hidden");
            }
        }
    }
    auto lines = scrubbed("INVITE sip:1002@pbx.example SIP/2.0\r\nFrom: <sip:1001@pbx.example>\r\n\r\nv=0\r\na=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:SECRETKEYSECRETKEY|2^20\r\n");
    check(!exposes(lines, "SECRETKEY") && exposes(lines, "a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:***"), "the SRTP key of a=crypto is hidden");
    check(exposes(lines, "From: <sip:1001@pbx.example>"), "other headers are kept");
}
void a_header_is_found_however_it_is_spaced() {
    std::string text = "SIP/2.0 200 OK\r\nCSeq : 7 REGISTER\r\nVia: SIP/2.0/UDP 192.0.2.10\r\n\r\n";
    auto get = [&](const char *name) { return ksip_text::sip_header(reinterpret_cast<const uint8_t *>(text.data()), text.size(), name); };
    check(get("cseq") == "7 REGISTER", "a name with space before the colon is found, case aside");
    check(get("Via") == "SIP/2.0/UDP 192.0.2.10", "an ordinary header is found");
    check(get("Contact").empty(), "a header that is not there is empty");
}
void dialog_info_is_read_as_xml_allows_it_and_not_when_cut_off() {
    using ksip_text::read_dialog_info;
    auto full = read_dialog_info("<?xml version=\"1.0\"?><dialog-info xmlns=\"urn:ietf:params:xml:ns:dialog-info\" version=\"3\" state=\"full\" entity=\"sip:701@pbx\">"
                                 "<dialog id=\"a1\"><state>confirmed</state></dialog><dialog id=\"b2\"><state>early</state></dialog></dialog-info>");
    check(full.readable && !full.partial && full.dialogs.size() == 2 && full.dialogs[0].first == "a1" && full.dialogs[0].second == "confirmed", "a full report with two dialogs");
    auto single = read_dialog_info("<dialog-info state='partial' version='4'><dialog id='a1'><state>terminated</state></dialog></dialog-info>");
    check(single.readable && single.partial && single.dialogs.size() == 1 && single.dialogs[0].first == "a1" && single.dialogs[0].second == "terminated", "single quotes are attribute values too");
    auto spaced = read_dialog_info("<dialog-info state = \"partial\"><dialog id = 'c3' ><state>trying</state></dialog></dialog-info>");
    check(spaced.readable && spaced.partial && spaced.dialogs.size() == 1 && spaced.dialogs[0].first == "c3", "spaces around = do not change the reading");
    auto prefixed = read_dialog_info("<di:dialog-info xmlns:di=\"urn:ietf:params:xml:ns:dialog-info\" state=\"full\"><di:dialog id=\"x\"><di:state>confirmed</di:state></di:dialog></di:dialog-info>");
    check(prefixed.readable && prefixed.dialogs.size() == 1 && prefixed.dialogs[0].second == "confirmed", "a namespace prefix is skipped");
    auto empty = read_dialog_info("<dialog-info state=\"full\"/>");
    check(empty.readable && empty.dialogs.empty(), "an empty full report reads as free");
    auto truncated = read_dialog_info("<dialog-info state=\"full\"><dialog id=\"a1\"><state>confirmed</state>");
    check(!truncated.readable && truncated.dialogs.empty(), "a body cut off before its closing tag is not a report");
    auto other = read_dialog_info("<presence><tuple><status><basic>open</basic></status></tuple></presence>");
    check(!other.readable, "another format is not a report");
    check(!read_dialog_info("").readable, "an empty body is not a report");
    // The whole body is checked before any of it counts.
    check(!read_dialog_info("<dialog-info version=\"1\"><dialog id=\"a\"><state>confirmed</state></dialog></dialog-info>").readable,
          "a report that does not say full or partial is not applied");
    check(!read_dialog_info("<dialog-info state=\"whole\"/>").readable, "an unknown kind of report is not applied");
    check(!read_dialog_info("<dialog-info state=\"full\"><dialog><state>confirmed</state></dialog></dialog-info>").readable, "a dialog without an id is not applied");
    check(!read_dialog_info("<dialog-info state=\"full\"><dialog id=\"a\"><state>confirmed</dialog-info>").readable, "an element left open inside is not applied");
    check(!read_dialog_info("<dialog-info state=\"full\"><dialog id=\"a\"><state>dancing</state></dialog></dialog-info>").readable, "an unknown dialog state is not applied");
    check(!read_dialog_info("<dialog-info state=\"full\"><dialog id=\"a\"></dialog></dialog-info>").readable, "a dialog without a state is not applied");
    check(!read_dialog_info("<dialog-info state=\"full\"></dialog-info><dialog-info state=\"full\"/>").readable, "two reports in one body are not applied");
    auto rich = read_dialog_info("<dialog-info state=\"full\" entity=\"sip:701@pbx\"><dialog id=\"a\" direction=\"recipient\"><state>confirmed</state>"
                                 "<local><identity>sip:701@pbx</identity></local><remote><identity display=\"x\">sip:1002@pbx</identity></remote></dialog></dialog-info>");
    check(rich.readable && rich.dialogs.size() == 1 && rich.dialogs[0].second == "confirmed", "local and remote parts are passed over when they close in order");
}
void numbers_and_lists_are_checked() {
    check(ksip_text::sip_uri("sip:sales@pbx.example") && ksip_text::sip_uri("SIPS:a@b") && !ksip_text::sip_uri("701"), "a URI is told by its scheme");
    check(ksip_text::token("*97#+", "*#+") && !ksip_text::token("70 1", "*#+") && !ksip_text::token("", "*#+"), "a number is digits and the allowed marks");
    check(ksip_text::escape_user("#31#") == "%2331%23", "the hash is escaped in a user part");
    check(ksip_text::codec_list("PCMU,opus,bogus,PCMU") == "PCMU/8000/1,opus/48000/1", "the codecs keep the app's order, once each, known ones only");
    check(ksip_text::codec_list("") == "opus/48000/1,G722/16000/1,PCMU/8000/1,PCMA/8000/1", "no choice means every codec");
    std::array<std::string, 30> values;
    check(ksip_text::parse_watch_list("701,,sip:park@pbx", values) && values[0] == "701" && values[1].empty() && values[2] == "sip:park@pbx", "a watch list with an empty slot");
    check(!ksip_text::parse_watch_list("701,701", values), "a number named twice is refused");
    check(!ksip_text::parse_watch_list("70 1", values), "a number with a space is refused");
    std::string mic, spk;
    check(ksip_text::parse_audio_devices("{mic},{spk}", 160, mic, spk) && mic == "{mic}" && spk == "{spk}", "the two endpoint ids");
    check(!ksip_text::parse_audio_devices("{mic}", 160, mic, spk), "one id is not enough");
}
} // namespace

int main() {
    digest_headers_are_hidden_however_they_are_written();
    a_header_is_found_however_it_is_spaced();
    dialog_info_is_read_as_xml_allows_it_and_not_when_cut_off();
    numbers_and_lists_are_checked();
    the_newest_player_has_the_stream_and_the_one_before_gets_it_back();
    a_start_that_fails_gives_the_stream_back_to_the_player_it_took_it_from();
    a_start_that_fails_with_nobody_to_hand_back_to_lets_the_streams_idle();
    a_restore_that_fails_too_lets_the_streams_idle();
    a_microphone_that_will_not_open_is_replaced_by_silence_until_the_source_goes();
    the_state_follows_the_microphone_from_silence_to_the_device_and_back();
    the_state_says_the_speaker_plays_after_a_failed_start_handed_back();
    a_failure_of_the_speaker_is_not_hidden_by_a_later_one_of_the_microphone();
    a_hand_back_that_fails_is_counted_as_the_speakers();
    a_restore_that_fails_after_a_failed_start_is_counted_too();
    the_microphone_is_opened_ahead_only_once_and_only_when_free();
    clearing_a_callback_waits_for_the_call_in_flight();
    clearing_from_inside_the_callback_does_not_wait_on_itself();
    a_late_set_after_the_clear_is_seen_by_the_next_call();
    a_recording_writes_both_sides_with_sizes_in_the_header();
    far_frames_beyond_the_buffer_are_dropped_and_counted();
    a_file_that_cannot_be_opened_is_reported_not_written();
    finishing_twice_is_once();
    this_side_is_kept_while_the_far_end_is_silent();
    samples_at_another_rate_are_brought_to_the_file_rate();
    a_gap_in_the_far_end_leaves_this_side_in_place();
    a_rate_change_in_the_middle_keeps_the_time_axis();
    a_side_that_starts_late_is_placed_where_the_clock_says();
    silence_for_a_gap_goes_after_the_samples_already_in_hand();
    std::printf("%s: %d failure(s)\n", failures ? "FAIL" : "PASS", failures);
    return failures ? 1 : 0;
}
