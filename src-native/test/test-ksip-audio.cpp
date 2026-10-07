// Unit tests of the audio module's parts that need no device, no baresip
// and no WebRTC: who gets the playout stream and when it is handed back
// (also after a start that fails), what a source does without a microphone,
// the alert sounds' bridge to their core, the callback slot that is cleared
// while a call runs, and the WAV recorder.
// Built and run by scripts/test/audio-module.ps1.
#include "ksip_audio/recorder.h"
#include "ksip_audio/session_core.h"
#include "ksip_audio/alert_adm.h"
#include "ksip_audio/inband_dtmf_tone.h"
#include "ksip/ksip_text.h"
#include "webrtc/callback_gate.h"
#include "webrtc/serving.h"

#include <algorithm>
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
    void set_device(const char *device) { name = device; }
};
struct Source {
    bool started = false;
};
// The bridge as the core sees it, deciding what device_selection.cc
// decides: a device that is not there (`absent`) has the default opened in
// its place and the start succeed, a device that is there but would not
// start (`broken`, by the name asked for or by the endpoint it opens) fails
// the start, and a start for the request a running stream serves takes it
// over as it is (ksip_audio::Serves), unless the device asked for is back.
struct FakeAdm : playback_session::Adm {
    std::vector<std::string> diary;
    void *playing = nullptr, *recording = nullptr;
    bool playout_on = false, recording_on = false;
    std::vector<std::string> broken, absent;
    std::string default_speaker = "default-speaker", default_microphone = "default-microphone";
    std::string playout_request, recording_request, playout_opened, recording_opened;
    static bool has(const std::vector<std::string> &list, const std::string &device) {
        return std::find(list.begin(), list.end(), device) != list.end();
    }
    // "default" is listed while any device is: `absent` with "default" in it
    // means no device at all, and then the default opens nothing either.
    bool listed(const char *device, bool) const { return !has(absent, device); }
    std::string opened(bool playout) override { return playout ? playout_opened : recording_opened; }
    std::string endpoint_for(const std::string &request, bool playout) const {
        return request == "default" || has(absent, request) ? (playout ? default_speaker : default_microphone) : request;
    }
    bool no_device_for(const std::string &request) const { return has(absent, "default") && (request == "default" || has(absent, request)); }
    // ksip_audio::Serves: a stream on an endpoint that is gone serves
    // nothing; otherwise a running stream for the same request is taken as
    // it is unless the device asked for is back, or the default asked for
    // stands for another endpoint now.
    bool serves(const std::string &request, bool playout) {
        const std::string on = opened(playout);
        if (!on.empty() && !listed(on.c_str(), playout)) return false;
        if (request == "default") return on == endpoint_for("default", playout);
        return on == request || !listed(request.c_str(), playout);
    }
    std::string default_endpoint(bool playout) const { return playout ? default_speaker : default_microphone; }
    int start_playout(const char *device, void *player) override {
        const std::string request = device;
        if (playout_on && playout_request == request && serves(request, true)) {
            diary.push_back("serve_playout " + request);
            playing = player;
            return 0;
        }
        diary.push_back("start_playout " + request);
        playout_on = false;
        playing = nullptr;
        playout_opened.clear();
        if (no_device_for(request)) return -5;
        const std::string endpoint = endpoint_for(request, true);
        if (has(broken, request) || has(broken, endpoint)) return -3;
        playing = player;
        playout_on = true;
        playout_request = request;
        playout_opened = endpoint;
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
        playout_opened.clear();
    }
    int start_recording(const char *device, void *source) override {
        const std::string request = device;
        if (recording_on && recording_request == request && serves(request, false)) {
            diary.push_back("serve_recording " + request);
            recording = source;
            return 0;
        }
        diary.push_back("start_recording " + request);
        recording_on = false;
        recording = nullptr;
        recording_opened.clear();
        if (no_device_for(request)) return -5;
        const std::string endpoint = endpoint_for(request, false);
        if (has(broken, request) || has(broken, endpoint)) return -3;
        recording = source;
        recording_on = true;
        recording_request = request;
        recording_opened = endpoint;
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
        recording_opened.clear();
    }
    // How many times a device was asked to start (not taken over as it was).
    size_t starts(const char *entry) const { return std::count(diary.begin(), diary.end(), std::string(entry)); }
};
struct FakeClock : playback_session::Clock {
    std::vector<std::string> diary;
    bool linger = false, handback = false, watch = false;
    bool &flag(playback_session::Timer which) {
        return which == playback_session::Timer::Linger ? linger : which == playback_session::Timer::HandBack ? handback : watch;
    }
    void start(playback_session::Timer which, uint64_t ms) override {
        flag(which) = true;
        const char *name = which == playback_session::Timer::Linger ? "linger " : which == playback_session::Timer::HandBack ? "handback " : "watch ";
        diary.push_back(name + std::to_string(ms));
    }
    void cancel(playback_session::Timer which) override { flag(which) = false; }
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
    size_t logged_times(const char *part) const {
        size_t n = 0;
        for (auto &l : log) if (l.find(part) != std::string::npos) ++n;
        return n;
    }
};
using playback_session::Outcome;

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
    f.adm.broken = {"speaker-broken", "default-speaker"};
    f.adm.diary.clear();
    check(f.core.take_playout(&call) == ENODEV, "the failed start is refused");
    check(f.adm.diary.size() == 4 && f.adm.diary[0] == "detach_playout" && f.adm.diary[1] == "start_playout speaker-broken" &&
              f.adm.diary[2] == "start_playout default" && f.adm.diary[3] == "start_playout speaker-a",
          "the old player is detached, the new start tried, the default tried in its place, the old start restored, in that order");
    check(f.adm.playing == &tone && tone.started && f.core.playout() == &tone, "the old player has its stream back");
    check(f.logged("failed speaker-broken -3") && f.logged("handed back after a failed start"), "the failure and the hand-back are on record");
    check(!f.clock.linger, "nothing idles while the old player plays");
    check(f.core.player_count() == 1, "the failed player is not kept");
}
void a_start_that_fails_with_nobody_to_hand_back_to_lets_the_streams_idle() {
    Fixture f;
    Player call{"speaker-broken"};
    f.adm.broken = {"speaker-broken", "default-speaker"};
    check(f.core.take_playout(&call) == ENODEV && f.core.playout() == nullptr, "the start fails and nobody has the stream");
    check(f.clock.linger, "the idle streams are left to close in their own time");
}
void a_restore_that_fails_too_leaves_the_player_the_owner_without_a_stream() {
    Fixture f;
    Player tone{"speaker-a"}, call{"speaker-broken"};
    f.core.take_playout(&tone);
    f.adm.broken = {"speaker-broken", "speaker-a", "default-speaker"};
    check(f.core.take_playout(&call) == ENODEV && !tone.started, "neither the new nor the old start works");
    check(f.core.playout() == &tone && !f.core.playing(), "the old player stays the owner, with no stream: a later try can start it");
    check(f.clock.linger, "the streams idle");
}
// A device that is there but would not start: the default in its place,
// as the bridge does for one that is not there; a call does not fail for it.
void a_speaker_that_would_not_start_has_the_default_in_its_place() {
    Fixture f;
    Player call{"speaker-broken"};
    f.adm.broken = {"speaker-broken"};
    check(f.core.take_playout(&call) == 0 && f.adm.playing == &call && call.started, "the player gets a stream");
    check(f.adm.playout_opened == "default-speaker" && std::string(call.device()) == "speaker-broken", "on the default, the device asked for staying asked for");
    check(f.logged("failed speaker-broken -3") && f.logged("the default plays in its place"), "the failure and the stand-in are on record");
    check(f.core.speaker_failures().count == 1, "the failure is counted");
    check(f.core.playing() && f.core.endpoint(true) == "default-speaker" && f.core.stand_in(true), "the state: playing, on the default, standing in");
}
void a_microphone_that_would_not_start_has_the_default_in_its_place() {
    Fixture f;
    Source mic;
    f.adm.broken = {"mic-broken"};
    check(f.core.take_source(&mic, "mic-broken") == 0 && mic.started && f.adm.recording == &mic, "the source records");
    check(f.adm.recording_opened == "default-microphone" && f.fallbacks.empty(), "on the default, not on silence");
    check(f.logged("the default records in its place") && f.core.microphone_failures().count == 1, "said and counted");
    check(f.core.input() == playback_session::Input::Device && f.core.endpoint(false) == "default-microphone" && f.core.stand_in(false),
          "the state: the device, the default standing in");
    f.core.source_gone(&mic);
}
void a_microphone_that_will_not_open_is_replaced_by_silence_until_the_source_goes() {
    Fixture f;
    Source mic;
    f.adm.broken = {"mic-broken", "default-microphone"};
    check(f.core.take_source(&mic, "mic-broken") == 0, "the call goes on without a microphone");
    check(f.fallbacks.size() == 1 && f.fallbacks[0] == &mic && !mic.started, "the fallback runs for the source");
    check(f.logged("failed mic-broken -3") && f.logged("failed default -3"), "the failures are on record, the default's too");
    check(f.core.endpoint(false).empty() && !f.core.stand_in(false), "the state names no endpoint for silence");
    Source next;
    check(f.core.take_source(&next, "mic-ok") == 0 && next.started && f.adm.recording == &next, "a new source with a working microphone takes over");
    check(f.fallbacks.empty(), "the fallback of the replaced source is stopped");
    f.core.source_gone(&next);
    check(f.adm.recording == nullptr && f.clock.linger, "the microphone is detached and idles");
}
// The devices chosen again during a call: the streams that are up move to
// them, for the same player and source.
void a_switch_during_a_call_moves_the_streams_that_are_up() {
    Fixture f;
    Player ring{"speaker-a"}, call{"speaker-a"};
    Source mic;
    f.core.take_playout(&ring);
    f.core.take_playout(&call);
    f.core.take_source(&mic, "mic-a");
    f.adm.diary.clear();
    f.log.clear();
    playback_session::Switch outcome = f.core.switch_devices("mic-b", "speaker-b");
    check(f.adm.diary.size() == 2 && f.adm.diary[0] == "start_playout speaker-b" && f.adm.diary[1] == "start_recording mic-b",
          "the speaker and the microphone of the call are opened again on the new devices");
    check(f.adm.playing == &call && call.started && f.adm.recording == &mic && mic.started, "for the same player and source");
    check(std::string(ring.device()) == "speaker-b", "a player waiting behind the call is moved too, for its hand-back");
    check(f.logged("speaker switched") && f.logged("microphone switched"), "both switches are said so");
    check(outcome.speaker == Outcome::Moved && outcome.microphone == Outcome::Moved, "both sides answer that they moved");
    f.adm.diary.clear();
    outcome = f.core.switch_devices("mic-b", "speaker-b");
    check(f.adm.starts("start_playout speaker-b") == 0 && f.adm.starts("start_recording mic-b") == 0, "the same devices again open nothing again");
    check(outcome.speaker == Outcome::Unchanged && outcome.microphone == Outcome::Unchanged, "and answer that nothing changed");
    f.adm.diary.clear();
    outcome = f.core.switch_devices(nullptr, nullptr);
    check(f.adm.diary.empty() && outcome.speaker == Outcome::NotUp && outcome.microphone == Outcome::NotUp, "a side another module has is left alone");
    Fixture idle;
    outcome = idle.core.switch_devices("mic-b", "speaker-b");
    check(outcome.speaker == Outcome::NotUp && outcome.microphone == Outcome::NotUp && idle.adm.diary.empty(), "with no call up there is nothing to move");
}
void a_switch_to_a_device_that_will_not_start_keeps_the_call_on_the_old_one() {
    Fixture f;
    Player call{"speaker-a"};
    Source mic;
    f.core.take_playout(&call);
    f.core.take_source(&mic, "mic-a");
    f.adm.broken = {"speaker-broken", "mic-broken"};
    f.adm.diary.clear();
    playback_session::Switch outcome = f.core.switch_devices("mic-broken", "speaker-broken");
    check(f.adm.playing == &call && call.started && std::string(call.device()) == "speaker-a", "the call keeps the speaker it had");
    check(f.adm.recording == &mic && mic.started && f.fallbacks.empty(), "and the microphone it had, not silence");
    check(f.logged("failed speaker-broken -3") && f.logged("failed mic-broken -3"), "the failures are on record");
    check(f.core.speaker_failures().count == 1 && f.core.microphone_failures().count == 1, "and counted, each on its side");
    check(outcome.speaker == Outcome::Kept && outcome.microphone == Outcome::Kept, "both sides answer that they kept what they had");
    check(!f.core.stand_in(true) && !f.core.stand_in(false), "the devices they had are what they ask for again: no stand-in");
    // Where the old microphone cannot be had either, the default stands in.
    f.adm.broken = {"mic-c", "mic-a"};
    outcome = f.core.switch_devices("mic-c", "speaker-a");
    check(mic.started && f.fallbacks.empty() && f.adm.recording_opened == "default-microphone", "with neither microphone, the call goes on with the default");
    check(outcome.microphone == Outcome::Kept && outcome.speaker == Outcome::Unchanged, "the microphone kept a stream, the speaker was left as it was");
    check(f.core.stand_in(false) && f.logged("the default, in place of the one that would not start"), "the default stands in for the microphone chosen");
    // And where the default cannot be had either, silence.
    f.adm.broken = {"mic-d", "mic-c", "default-microphone"};
    outcome = f.core.switch_devices("mic-d", "speaker-a");
    check(!mic.started && f.fallbacks.size() == 1 && f.fallbacks[0] == &mic && outcome.microphone == Outcome::Down, "with nothing to record from, silence");
    f.adm.broken.clear();
    outcome = f.core.switch_devices("mic-e", "speaker-a");
    check(mic.started && f.fallbacks.empty() && f.adm.recording == &mic && outcome.microphone == Outcome::Moved, "a source on silence takes the next microphone chosen");
}
// A speaker whose starts all failed is the owner still, and a later choice
// starts it: the same device again (it may work now) or another.
void a_speaker_whose_starts_all_failed_is_started_by_a_later_choice() {
    Fixture f;
    Player call{"speaker-a"};
    f.core.take_playout(&call);
    f.adm.broken = {"speaker-b", "speaker-a", "default-speaker"};
    playback_session::Switch outcome = f.core.switch_devices(nullptr, "speaker-b");
    check(outcome.speaker == Outcome::Down && !f.core.playing() && f.core.playout() == &call, "nothing starts: the call has no playout, and keeps its owner");
    check(f.core.endpoint(true).empty(), "the state names no endpoint");
    f.adm.broken.clear();
    outcome = f.core.switch_devices(nullptr, "speaker-b");
    check(outcome.speaker == Outcome::Moved && f.core.playing() && f.adm.playing == &call && f.adm.playout_opened == "speaker-b",
          "the same device chosen again is tried again, and plays");
    f.adm.broken = {"speaker-c", "speaker-b", "default-speaker"};
    f.core.switch_devices(nullptr, "speaker-c");
    f.adm.broken.clear();
    outcome = f.core.switch_devices(nullptr, "speaker-d");
    check(outcome.speaker == Outcome::Moved && f.core.playing() && f.adm.playout_opened == "speaker-d", "another device chosen after the failures plays");
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
    f.adm.broken = {"mic-broken", "default-microphone"};
    f.core.take_source(&first, "mic-broken");
    check(f.core.input() == Input::Silence && f.core.microphone_failures().count == 2, "a microphone that would not open, nor the default: the input is silence, two failures");
    check(f.core.microphone_failures().last_result == -3 && f.core.speaker_failures().count == 0, "the failure is the microphone's, with the bridge's result");
    f.core.take_source(&second, "mic-broken");
    check(f.core.input() == Input::Silence && f.core.microphone_failures().count == 4, "silence replaced by silence: still silence, the failures counted again");
    f.core.take_source(&third, "mic-ok");
    check(f.core.input() == Input::Device && f.core.microphone_failures().count == 4, "a later start that works: the input is the device, and the failures stay counted");
    f.adm.broken.push_back("mic-late");
    f.core.take_source(&fourth, "mic-late");
    check(f.core.input() == Input::Silence && f.core.microphone_failures().count == 6, "the device replaced by one that fails: silence again");
    StopAll(f, fourth);
    check(f.core.input() == Input::None, "the source gone: no input, whatever it was");
}
void the_state_says_the_speaker_plays_after_a_failed_start_handed_back() {
    Fixture f;
    Player tone{"speaker-a"}, call{"speaker-broken"}, later{"speaker-a"};
    f.core.take_playout(&tone);
    f.adm.broken = {"speaker-broken", "default-speaker"};
    f.core.take_playout(&call);
    check(f.core.playout() == &tone && f.core.playing() && f.core.speaker_failures().count == 2 && f.core.speaker_failures().last_result == -3,
          "the failed starts are counted as the speaker's, and the tone plays on");
    f.core.take_playout(&later);
    check(f.core.playout() == &later && f.core.playing() && f.core.speaker_failures().count == 2, "a later start that works: the speaker plays, the failures stay counted");
    f.core.playout_gone(&later);
    f.core.hand_back();
    f.core.playout_gone(&tone);
    f.core.hand_back();
    check(f.core.playout() == nullptr && !f.core.playing() && f.core.speaker_failures().count == 2, "every player gone: nothing plays, and nothing failed in going");
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
    f.adm.broken = {"speaker-a"};
    f.core.playout_gone(&call);
    f.log.clear();
    f.core.hand_back();
    check(f.core.playout() == &tone && f.core.playing() && f.adm.playout_opened == "default-speaker", "the player left has the stream back on the default");
    check(f.core.speaker_failures().count == 1 && f.logged("failed speaker-a -3"), "the failed hand-back is counted and logged as the speaker's");
    // With the default no better, the player left is the owner without a stream.
    Fixture g;
    Player tone2{"speaker-a"}, call2{"speaker-b"};
    g.core.take_playout(&tone2);
    g.core.take_playout(&call2);
    g.adm.broken = {"speaker-a", "default-speaker"};
    g.core.playout_gone(&call2);
    g.core.hand_back();
    check(g.core.playout() == &tone2 && !g.core.playing() && g.clock.linger, "the player left cannot have the stream back, stays the owner, and the streams idle");
    check(g.core.speaker_failures().count == 2, "both failed starts are counted");
}
void a_restore_that_fails_after_a_failed_start_is_counted_too() {
    Fixture f;
    Player tone{"speaker-a"}, call{"speaker-broken"};
    f.core.take_playout(&tone);
    f.adm.broken = {"speaker-broken", "speaker-a", "default-speaker"};
    f.core.take_playout(&call);
    check(f.core.speaker_failures().count == 3 && f.logged("failed speaker-broken -3") && f.logged("failed default -3") && f.logged("failed speaker-a -3"),
          "the new start, the default in its place and the restore of the old one are all counted and logged");
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
    f.adm.diary.clear();
    f.core.open_microphone_ahead("mic-ok");
    check(f.adm.diary.empty(), "a microphone a source has is left alone");
    StopAll(f, mic);
}
// ---- the watch: a device that was not there, back while the call is up
// A device chosen again by the person while the default stands in for it
// is looked at afresh: the bridge opens it when it is back.
void the_same_device_chosen_again_takes_the_call_back_from_the_default_at_once() {
    Fixture f;
    Player call{"speaker-x"};
    f.adm.absent = {"speaker-x"};
    f.core.take_playout(&call);
    f.adm.diary.clear();
    playback_session::Switch outcome = f.core.switch_devices(nullptr, "speaker-x");
    check(outcome.speaker == Outcome::Unchanged && f.adm.starts("start_playout speaker-x") == 0, "while it is not there, nothing changes");
    f.adm.absent.clear();
    outcome = f.core.switch_devices(nullptr, "speaker-x");
    check(outcome.speaker == Outcome::Moved && f.adm.playout_opened == "speaker-x" && !f.core.stand_in(true), "once it is there, the stream is opened on it");
}
// The default moved to an endpoint that would not start: the call stays on
// A stream the owner started, which the bridge says is not running (WebRTC
// restarted it when its device went, and the restart failed; the endpoint
// it opened for that is still noted): opened again after two looks, as a
// stream on an endpoint that is gone is.
void a_stream_webrtc_could_not_restart_is_opened_again() {
    Fixture f;
    Player call{"speaker-x"};
    Source mic;
    f.core.take_playout(&call);
    f.core.take_source(&mic, "mic-x");
    f.adm.playout_on = false;
    f.adm.recording_on = false;
    f.adm.diary.clear();
    f.core.watch_tick();
    check(f.adm.diary.empty(), "the first look leaves it alone");
    f.core.watch_tick();
    check(f.adm.starts("start_playout speaker-x") == 1 && f.adm.starts("start_recording mic-x") == 1 && f.adm.playout_on && f.adm.recording_on, "the second opens both streams again");
    check(f.logged("speaker stream stopped") && f.logged("microphone stream stopped"), "said so");
    check(f.core.speaker_failures().count == 0 && f.core.microphone_failures().count == 0 && call.started && mic.started, "no failed start of this core: the streams are up again");
    StopAll(f, mic);
}
// ---- the alert sounds' bridge (alert_adm.h), against a render that only keeps a diary
// The render as alert_render.h's is to the core's bridge: opened on an
// endpoint by its own id (the default asked for included), with the result
// known at once; ended on its own when the stream fails under it.
struct FakeRender {
    // The speakers there, which of them the default is, and those whose
    // open fails: the test's to set.
    static inline std::vector<std::string> listed;
    static inline std::string default_id;
    static inline std::vector<std::string> failing;
    static inline int opens = 0;
    std::string id;
    bool ended_ = false;
    static bool usable(const char *device) {
        const std::string asked = device;
        return asked == "default" ? !listed.empty() : FakeAdm::has(listed, asked);
    }
    static std::string default_endpoint() { return default_id; }
    int open(const std::string &endpoint, void *) {
        ++opens;
        const std::string on = endpoint == "default" ? default_id : endpoint;
        if (on.empty() || FakeAdm::has(failing, on)) return ENODEV;
        id = on;
        ended_ = false;
        return 0;
    }
    void close() {
        ended_ = false;
        id.clear();
    }
    bool ended() const { return ended_; }
};
struct AlertFixture {
    alert_player::RenderAdm<FakeRender> adm;
    FakeClock clock;
    std::vector<std::string> log;
    playback_session::Core<Player, Source> core;
    AlertFixture()
        : adm({FakeRender::usable, [this](const std::string &asked) { log.push_back("not there " + asked); }, FakeRender::default_endpoint}),
          core(adm, clock,
               {[this](const char *line) { log.emplace_back(line); },
                [this](bool, const char *device, int result) { log.push_back(std::string("failed ") + device + " " + std::to_string(result)); },
                [](Source *) {}, [](Source *) {}}) {
        FakeRender::listed = {"speaker-a", "speaker-b"};
        FakeRender::default_id = "speaker-a";
        FakeRender::failing.clear();
        FakeRender::opens = 0;
    }
    bool logged(const char *part) const {
        for (auto &l : log) if (l.find(part) != std::string::npos) return true;
        return false;
    }
    // A speaker goes, as Windows and the stream see it: no longer listed,
    // the default elsewhere, and the stream on it ended.
    void gone(const char *speaker, const char *default_now) {
        auto &listed = FakeRender::listed;
        listed.erase(std::remove(listed.begin(), listed.end(), speaker), listed.end());
        FakeRender::default_id = default_now;
        if (adm.render.id == speaker) adm.render.ended_ = true;
    }
};
// The alert on the default asked for is on that speaker's own id, so that
// A start that fails is known as it fails, counted with its result, and the
// default tried in the speaker's place; failing there too leaves the alert
// down, both failures counted.
void an_alert_start_that_fails_is_counted_and_the_default_tried() {
    AlertFixture f;
    FakeRender::failing = {"speaker-b"};
    Player ring{"speaker-b"};
    check(f.core.take_playout(&ring) == 0 && f.core.endpoint(true) == "speaker-a" && f.core.stand_in(true), "the default plays in place of the speaker that would not start");
    check(f.core.speaker_failures().count == 1 && f.core.speaker_failures().last_result == ENODEV && f.logged("failed speaker-b"), "the failed start is counted, with its result");
    f.core.playout_gone(&ring);
    f.core.hand_back();
    FakeRender::failing = {"speaker-a", "speaker-b"};
    Player again{"speaker-b"};
    check(f.core.take_playout(&again) == ENODEV && !f.core.playing() && f.core.endpoint(true).empty(), "neither starting, nothing plays");
    check(f.core.speaker_failures().count == 3, "both counted");
}
// The last speaker gone under the alert: nothing plays, no start is tried
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

// ---- which running stream serves a request (the bridge's Serves, webrtc/serving.h)
using ksip_audio_bridge::Serving;
using ksip_audio_bridge::serving;
void a_stream_on_an_endpoint_that_is_gone_or_unknown_serves_nothing() {
    const std::vector<std::string> listed{"{a}", "{b}"};
    check(serving("{a}", "{a}", listed, "{b}") == Serving::kKept, "on the device asked for: kept");
    check(serving("{a}", "", listed, "{b}") == Serving::kEndpointGone, "the endpoint opened is not known: opened again");
    check(serving("{a}", "{c}", listed, "{b}") == Serving::kEndpointGone, "the endpoint in use is not listed any more: opened again");
    check(serving("default", "{a}", {}, "") == Serving::kEndpointGone, "nothing listed at all: opened again (and fails, to the fallbacks)");
}
void the_default_asked_for_follows_the_default_moving() {
    const std::vector<std::string> listed{"{a}", "{b}"};
    check(serving("default", "{a}", listed, "{a}") == Serving::kKept, "on the default: kept");
    check(serving("default", "{a}", listed, "{b}") == Serving::kDefaultMoved, "the default is another now: opened on it");
    check(serving("default", "{a}", listed, "") == Serving::kKept, "no default known: kept where it is");
}
void a_device_that_is_back_has_the_call_again_and_one_still_gone_leaves_it_on_the_default() {
    check(serving("{u}", "{a}", {"{a}"}, "{a}") == Serving::kKept, "the device asked for is not there: the default in its place is kept");
    check(serving("{u}", "{a}", {"{a}", "{u}"}, "{a}") == Serving::kDeviceBack, "the device asked for is back: opened on it again");
    check(serving("{u}", "{b}", {"{a}", "{u}", "{b}"}, "{a}") == Serving::kDeviceBack, "a stream WebRTC moved by itself goes back to its device too");
    check(serving("{u}", "{a}", {"{a}", "{u}"}, "{u}") == Serving::kDeviceBack, "the device back as the default too: opened on it, as asked");
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
    std::array<std::string, ksip_text::WATCH_COUNT> values;
    check(ksip_text::parse_watch_list("701,,sip:park@pbx", values) && values[0] == "701" && values[1].empty() && values[2] == "sip:park@pbx", "a watch list with an empty slot");
    check(!ksip_text::parse_watch_list("701,701", values), "a number named twice is refused");
    check(!ksip_text::parse_watch_list("70 1", values), "a number with a space is refused");
    std::string full;
    for (size_t n = 0; n < ksip_text::WATCH_COUNT; n++) full += (n ? ",7" : "7") + std::to_string(100 + n);
    check(ksip_text::parse_watch_list(full.c_str(), values) && values[ksip_text::WATCH_COUNT - 1] == "7153", "a number for every button is taken");
    check(!ksip_text::parse_watch_list((full + ",7999").c_str(), values), "one more than the buttons is refused");
    std::string mic, spk;
    check(ksip_text::parse_audio_devices("{mic},{spk}", 160, mic, spk) && mic == "{mic}" && spk == "{spk}", "the two endpoint ids");
    check(!ksip_text::parse_audio_devices("{mic}", 160, mic, spk), "one id is not enough");
}
// The keepalives follow the registrar only on the 2xx to the REGISTER sent:
// the same Call-ID, CSeq and top Via branch, compact names read too.
void only_the_answer_to_the_register_sent_is_taken() {
    auto bytes = [](const std::string &s) { return reinterpret_cast<const uint8_t *>(s.data()); };
    const std::string sent =
        "REGISTER sip:pbx.example SIP/2.0\r\n"
        "Via: SIP/2.0/UDP 192.0.2.20:5060;branch=z9hG4bKabc123;rport\r\n"
        "Call-ID: reg-1@192.0.2.20\r\n"
        "CSeq: 7  REGISTER\r\n\r\n";
    auto ids = ksip_text::request_ids(bytes(sent), sent.size(), "REGISTER");
    check(ids.call_id == "reg-1@192.0.2.20" && ids.cseq == "7 REGISTER" && ids.branch == "z9hG4bKabc123", "a REGISTER sent is known by its Call-ID, CSeq and Via branch");
    const std::string invite = "INVITE sip:1001@pbx.example SIP/2.0\r\nVia: SIP/2.0/UDP 192.0.2.20;branch=z9hG4bKx\r\nCall-ID: c\r\nCSeq: 1 INVITE\r\n\r\n";
    check(ksip_text::request_ids(bytes(invite), invite.size(), "REGISTER").call_id.empty(), "another request is not taken for a REGISTER");
    auto answer = [&](const std::string &status, const std::string &via, const std::string &callid, const std::string &cseq) {
        std::string a = "SIP/2.0 " + status + "\r\n" + via + "\r\n" + callid + "\r\n" + cseq + "\r\n\r\n";
        return ksip_text::is_success_answer(ids, bytes(a), a.size());
    };
    const std::string via = "Via: SIP/2.0/UDP 192.0.2.20:5060;branch=z9hG4bKabc123;rport=5060;received=192.0.2.20";
    check(answer("200 OK", via, "Call-ID: reg-1@192.0.2.20", "CSeq: 7 REGISTER"), "its 200 OK is taken, with the parameters the registrar adds");
    check(answer("200 OK", "v: SIP/2.0/UDP 192.0.2.20:5060;branch=z9hG4bKabc123", "i: reg-1@192.0.2.20", "CSeq: 7 REGISTER"), "the compact names of Via and Call-ID are read");
    check(!answer("200 OK", via, "Call-ID: unrelated", "CSeq: 7 REGISTER"), "another Call-ID is not taken");
    check(!answer("200 OK", via, "Call-ID: reg-1@192.0.2.20", "CSeq: 99 REGISTER"), "another CSeq is not taken");
    check(!answer("200 OK", "Via: SIP/2.0/UDP 192.0.2.20:5060;branch=z9hG4bKother", "Call-ID: reg-1@192.0.2.20", "CSeq: 7 REGISTER"), "another branch is not taken");
    check(!answer("401 Unauthorized", via, "Call-ID: reg-1@192.0.2.20", "CSeq: 7 REGISTER"), "an answer that is not 2xx is not taken");
    check(!ksip_text::is_success_answer({}, bytes(sent), sent.size()), "nothing is taken before a REGISTER was sent");
    // A header may be folded over lines that start with white space (RFC 3261
    // 7.3.1): the Via's branch and the CSeq still match.
    const std::string folded = "SIP/2.0 200 OK\r\nVia: SIP/2.0/UDP 192.0.2.20:5060;\r\n branch=z9hG4bKabc123;rport=5060\r\n"
                               "Call-ID: reg-1@192.0.2.20\r\nCSeq: 7\r\n\tREGISTER\r\n\r\n";
    check(ksip_text::is_success_answer(ids, bytes(folded), folded.size()), "a 200 OK with its Via and CSeq folded over lines is taken");
    check(ksip_text::sip_header(bytes(folded), folded.size(), "Via") == "SIP/2.0/UDP 192.0.2.20:5060; branch=z9hG4bKabc123;rport=5060",
          "a folded header comes as one value, the fold one space");
    const std::string tricky = "SIP/2.0 200 OK\r\nContact: <sip:1001@192.0.2.20>;\r\n expires: 300\r\nExpires: 60\r\n\r\n";
    check(ksip_text::sip_header(bytes(tricky), tricky.size(), "Expires") == "60" && ksip_text::sip_header(bytes(tricky), tricky.size(), "expires ") .empty(),
          "a continuation line is never taken for a header of its own");
}
// The power of one frequency in a run of samples (Goertzel), as the square
// of its amplitude.
double tone_power(const std::vector<double> &samples, double frequency, unsigned srate) {
    const double k = 2 * std::cos(2 * 3.14159265358979323846 * frequency / srate);
    double s1 = 0, s2 = 0;
    for (double x : samples) {
        const double s0 = x + k * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    const double n = double(samples.size());
    return (s1 * s1 + s2 * s2 - k * s1 * s2) * 4 / (n * n);
}
void an_inband_digit_is_its_two_tones_for_as_long_as_the_rules_ask() {
    const double lows[] = {697, 770, 852, 941}, highs[] = {1209, 1336, 1477, 1633};
    // What KSIP sends: a 100 ms tone and a 100 ms pause (what the settings
    // and the ADMX say), whatever the rate.
    check(inband_dtmf::tone_samples(8000) == 800 && inband_dtmf::period_samples(8000) == 1600 && inband_dtmf::tone_samples(48000) == 4800 && inband_dtmf::period_samples(48000) == 9600,
          "a digit is a 100 ms tone and a 100 ms pause");
    for (unsigned srate : {8000u, 16000u, 48000u}) {
        // MIC Notice No. 357 of 2024, Appended Table 2: tone 50 ms or more,
        // pause 30 ms or more, period 120 ms or more.
        check(inband_dtmf::tone_samples(srate) * 1000 / srate >= 50, "the tone lasts at least 50 ms");
        check((inband_dtmf::period_samples(srate) - inband_dtmf::tone_samples(srate)) * 1000 / srate >= 30, "the pause lasts at least 30 ms");
        check(inband_dtmf::period_samples(srate) * 1000 / srate >= 120, "a digit's period is at least 120 ms");
        for (char digit : std::string("0123456789*#")) {
            double low = 0, high = 0;
            check(inband_dtmf::frequencies(digit, low, high), "a key of the keypad is a digit");
            std::vector<double> tone, pause;
            for (uint64_t i = 0; i < inband_dtmf::period_samples(srate); ++i)
                (i < inband_dtmf::tone_samples(srate) ? tone : pause).push_back(inband_dtmf::sample(digit, i, srate));
            // Its own two frequencies and none of the other six.
            const double lp = tone_power(tone, low, srate), hp = tone_power(tone, high, srate);
            double stray = 0;
            for (double f : lows) if (f != low) stray = std::max(stray, tone_power(tone, f, srate));
            for (double f : highs) if (f != high) stray = std::max(stray, tone_power(tone, f, srate));
            check(lp > 100 * stray && hp > 100 * stray, "a digit sounds its own two frequencies, not the others");
            // The high group 2 dB over the low, within the table's 5 dB, the low never above.
            const double twist = 10 * std::log10(hp / lp);
            check(twist > 1.5 && twist < 2.5, "the high tone is 2 dB above the low one");
            check(std::all_of(pause.begin(), pause.end(), [](double x) { return x == 0; }), "the pause is silence");
            check(std::all_of(tone.begin(), tone.end(), [](double x) { return std::fabs(x) < 1; }), "the tone never clips");
        }
    }
    double low = 0, high = 0;
    check(!inband_dtmf::frequencies('E', low, high) && !inband_dtmf::frequencies(0, low, high), "anything else is not a digit");
}
} // namespace

int main(int argc, char **argv) {
    // --no-clock: without the three recorder tests that time themselves by
    // the wall clock, for runs under the sanitizers, whose slowness moves
    // what those measure.
    const bool clock = !(argc >= 2 && std::strcmp(argv[1], "--no-clock") == 0);
    digest_headers_are_hidden_however_they_are_written();
    a_header_is_found_however_it_is_spaced();
    dialog_info_is_read_as_xml_allows_it_and_not_when_cut_off();
    numbers_and_lists_are_checked();
    only_the_answer_to_the_register_sent_is_taken();
    an_inband_digit_is_its_two_tones_for_as_long_as_the_rules_ask();
    the_newest_player_has_the_stream_and_the_one_before_gets_it_back();
    a_start_that_fails_gives_the_stream_back_to_the_player_it_took_it_from();
    a_start_that_fails_with_nobody_to_hand_back_to_lets_the_streams_idle();
    a_restore_that_fails_too_leaves_the_player_the_owner_without_a_stream();
    a_speaker_that_would_not_start_has_the_default_in_its_place();
    a_microphone_that_would_not_start_has_the_default_in_its_place();
    a_microphone_that_will_not_open_is_replaced_by_silence_until_the_source_goes();
    a_switch_during_a_call_moves_the_streams_that_are_up();
    a_switch_to_a_device_that_will_not_start_keeps_the_call_on_the_old_one();
    a_speaker_whose_starts_all_failed_is_started_by_a_later_choice();
    the_state_follows_the_microphone_from_silence_to_the_device_and_back();
    the_state_says_the_speaker_plays_after_a_failed_start_handed_back();
    a_failure_of_the_speaker_is_not_hidden_by_a_later_one_of_the_microphone();
    a_hand_back_that_fails_is_counted_as_the_speakers();
    a_restore_that_fails_after_a_failed_start_is_counted_too();
    the_microphone_is_opened_ahead_only_once_and_only_when_free();
    a_stream_webrtc_could_not_restart_is_opened_again();
    the_same_device_chosen_again_takes_the_call_back_from_the_default_at_once();
    an_alert_start_that_fails_is_counted_and_the_default_tried();
    clearing_a_callback_waits_for_the_call_in_flight();
    clearing_from_inside_the_callback_does_not_wait_on_itself();
    a_late_set_after_the_clear_is_seen_by_the_next_call();
    a_stream_on_an_endpoint_that_is_gone_or_unknown_serves_nothing();
    the_default_asked_for_follows_the_default_moving();
    a_device_that_is_back_has_the_call_again_and_one_still_gone_leaves_it_on_the_default();
    a_recording_writes_both_sides_with_sizes_in_the_header();
    far_frames_beyond_the_buffer_are_dropped_and_counted();
    a_file_that_cannot_be_opened_is_reported_not_written();
    finishing_twice_is_once();
    this_side_is_kept_while_the_far_end_is_silent();
    samples_at_another_rate_are_brought_to_the_file_rate();
    a_rate_change_in_the_middle_keeps_the_time_axis();
    if (clock) {
        a_gap_in_the_far_end_leaves_this_side_in_place();
        a_side_that_starts_late_is_placed_where_the_clock_says();
        silence_for_a_gap_goes_after_the_samples_already_in_hand();
    }
    std::printf("%s: %d failure(s)%s\n", failures ? "FAIL" : "PASS", failures, clock ? "" : " (the clock tests left out)");
    return failures ? 1 : 0;
}
