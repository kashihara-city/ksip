// The decisions of the audio session, with nothing real behind them: who
// has the playout stream and the microphone, what happens when a player or
// a source goes, when a start fails, when the devices chosen change under a
// call, and when the streams are left idle. The device bridge, the timers
// and the fallback thread are asked for through the interfaces below, so
// that the module can put the WebRTC bridge and libre's timers behind them,
// and a test can put fakes.
//
// Three things are kept apart. The device asked for (a player's device(),
// source_device) is the choice as saved, "default" included, and is what the
// bridge is asked to open. The endpoint the bridge opened for it (Adm::opened)
// may be another: the default in place of a device that is not there, or
// that would not start, or the one WebRTC moved to when the device in use
// went away. And whether the stream is up at all (started) is a third thing:
// a player or a source stays the owner of its side when its start failed, so
// that a later try (another device chosen, the device back) can start it.
//
// Whether a device is there, is back, or the default moved is not looked at
// here: the app watches Windows' devices (phone_actor.rs) and, on a change,
// hands the devices chosen to the module again (ksip_audio_devices), which
// comes to switch_devices; the bridge then takes a stream that still serves
// its device as it is and opens the others anew. Asking Windows from
// baresip's thread stalled the calls' audio on a machine with many devices.
// What is looked at here, once a second, is only whether a stream the owner
// started still runs (WebRTC's own restart of it after its device went may
// have failed), which costs nothing to ask.
//
// Threads: the Core is not thread-safe; it is called on one thread (baresip's
// main thread in the module, the test's thread in the tests), and the
// callbacks the Adm runs on the audio threads never call back into it.
#pragma once
#include <algorithm>
#include <cerrno>
#include <cstdint>
#include <functional>
#include <string>
#include <vector>

namespace playback_session {
// odict's hash buckets: a nominal size for the few entries written here.
constexpr uint32_t kDictBuckets = 8;
// What the session asks of the device bridge. A player or a source is
// named by its address, which the bridge hands to the render or capture
// callback as its argument; a null source means frames go nowhere.
struct Adm {
    virtual ~Adm() = default;
    virtual int start_playout(const char *device, void *player) = 0;
    virtual void detach_playout() = 0;
    virtual bool playout_running() = 0;
    virtual void stop_playout() = 0;
    virtual int start_recording(const char *device, void *source) = 0;
    virtual void detach_recording() = 0;
    virtual bool recording_running() = 0;
    virtual void stop_recording() = 0;
    // The endpoint the side's stream opened, whichever way it was chosen
    // (the device asked for, the default in its place, the one WebRTC moved
    // to by itself); empty while no stream is open on the side.
    virtual std::string opened(bool playout) = 0;
};
enum class Timer { Linger, HandBack, Watch };
// Where the current call's microphone comes from: no source, the device, or
// the timed silence that stands in for a device that would not start.
enum class Input { None, Device, Silence };
// How one side came out of a switch of devices (switch_devices).
enum class Outcome {
    NotUp,     // no stream of this module has the side
    Unchanged, // the stream serves the device asked for as it is
    Moved,     // the stream was opened on the device asked for
    Kept,      // that device would not start: the stream goes on, on the one it had or on the default
    Down,      // nothing would start: no playout, or the microphone is silence
};
inline const char *outcome_name(Outcome outcome) {
    static const char *const names[] = {"not_up", "unchanged", "moved", "kept", "down"};
    return names[static_cast<int>(outcome)];
}
struct Switch {
    Outcome microphone = Outcome::NotUp;
    Outcome speaker = Outcome::NotUp;
};
struct Clock {
    virtual ~Clock() = default;
    virtual void start(Timer which, uint64_t ms) = 0;
    virtual void cancel(Timer which) = 0;
};
// How long the streams stay open after their last user has gone. Opening a
// device takes a second or more on some hardware, and a call waits for it: the
// ACK goes out once the audio is up. The ringback tone hands its stream to the
// call, and a call that ends hands its streams to the next one within this
// time; after it the devices are closed, so nothing is held while idle.
constexpr uint64_t kLingerMs = 3000;
// How often a side with an owner looks whether the stream its owner started
// still runs (watch_tick). A stream found stopped at two looks in a row is
// opened again: WebRTC's own restart, on its audio thread, is over by then.
constexpr uint64_t kWatchMs = 1000;

// `Play` has `bool started`, `const char *device()` and `set_device(const
// char *)`; `Source` has `bool started`. The module's auplay_st and ausrc_st
// do; a test's records do too.
template <class Play, class Source>
class Core {
public:
    struct Hooks {
        std::function<void(const char *)> log;
        // A device that would not start: which side, which device, the bridge's result.
        std::function<void(bool playout, const char *device, int result)> start_failed;
        // The microphone that would not open is replaced by timed silence.
        std::function<void(Source *)> start_fallback;
        std::function<void(Source *)> stop_fallback;
    };
    Core(Adm &adm, Clock &clock, Hooks hooks) : adm(adm), clock(clock), hooks(std::move(hooks)) {}

    // A new player takes the stream from whoever has it: on its device, or
    // on the default when that would not start. Returns 0, or ENODEV when
    // neither would; then the caller frees the player, and the one that was
    // detached has its stream back if it could be restarted, so that what
    // was playing goes on instead of falling silent.
    int take_playout(Play *fresh) {
        clock.cancel(Timer::Linger);
        clock.cancel(Timer::HandBack);
        Play *previous = nullptr;
        if (active_playout) {
            adm.detach_playout();
            active_playout->started = false;
            previous = active_playout;
            active_playout = nullptr;
        }
        playout_stopped_seen = false;
        const bool running = adm.playout_running();
        Tried tried;
        if (open_playout(fresh, fresh->device(), tried)) {
            players.push_back(fresh);
            active_playout = fresh;
            hooks.log(running && adm.playout_running() ? "ksip_audio: WebRTC ADM playout taken over\n" : "ksip_audio: WebRTC ADM playout started\n");
            keep_watching();
            return 0;
        }
        if (previous) {
            // The owner either way, so that a later try (another device
            // chosen, the device back) can start it.
            active_playout = previous;
            if (open_playout(previous, previous->device(), tried)) {
                hooks.log("ksip_audio: WebRTC ADM playout handed back after a failed start\n");
                keep_watching();
                return ENODEV;
            }
        }
        keep_warm();
        return ENODEV;
    }
    // A player goes. If it had the stream, the stream is detached and handed
    // back once the event that took the player away has been dealt with: a
    // tone the menu stops in the same breath must not get the stream for an
    // instant on its way out.
    void playout_gone(Play *gone) {
        players.erase(std::remove(players.begin(), players.end(), gone), players.end());
        if (gone != active_playout) return;
        active_playout = nullptr;
        playout_stopped_seen = false;
        if (gone->started) adm.detach_playout();
        clock.start(Timer::HandBack, 0);
    }
    // Timer::HandBack fired: the newest player left gets the stream back, on
    // its device or on the default; it is the owner even when neither would
    // start, for a later try.
    void hand_back() {
        if (active_playout) return;
        if (!players.empty()) {
            Play *previous = players.back();
            active_playout = previous;
            Tried tried;
            if (open_playout(previous, previous->device(), tried)) {
                hooks.log("ksip_audio: WebRTC ADM playout handed back\n");
                keep_watching();
                return;
            }
        }
        keep_warm();
        keep_watching();
    }
    // Timer::Linger fired: streams nobody uses are closed.
    void stop_idle() {
        if (!playing() && adm.playout_running()) adm.stop_playout();
        if (input() != Input::Device && adm.recording_running()) adm.stop_recording();
        hooks.log("ksip_audio: idle audio streams closed\n");
    }
    // The microphone is opened as soon as the speaker is, that is while the
    // ringback tone plays, so that it is up by the time the call is answered.
    // What it captures until then is dropped.
    void open_microphone_ahead(const char *device) {
        if (active_source || adm.recording_running()) return;
        if (adm.start_recording(device, nullptr) == 0) hooks.log("ksip_audio: microphone opened ahead of the call\n");
    }
    // A new source takes the microphone from whoever has it: the device, or
    // the default when that would not start, or the fallback's silence when
    // neither would; the call goes on either way.
    int take_source(Source *fresh, const char *device) {
        clock.cancel(Timer::Linger);
        if (active_source) {
            adm.detach_recording();
            active_source->started = false;
            hooks.stop_fallback(active_source);
            active_source = nullptr;
        }
        source_stopped_seen = false;
        source_device = device;
        const bool running = adm.recording_running();
        active_source = fresh;
        Tried tried;
        if (open_source(fresh, source_device, tried)) {
            hooks.log(running && adm.recording_running() ? "ksip_audio: WebRTC ADM recording taken over, APM running\n"
                                                        : "ksip_audio: WebRTC ADM recording and APM started\n");
        } else {
            hooks.start_fallback(fresh);
        }
        keep_watching();
        return 0;
    }
    // A source goes; its fallback is the caller's to stop first.
    void source_gone(Source *gone) {
        if (gone != active_source) return;
        if (gone->started) adm.detach_recording();
        active_source = nullptr;
        source_stopped_seen = false;
        keep_warm();
        keep_watching();
    }
    // The devices chosen are handed over again (the person chose them, or
    // the app saw Windows' devices change): each side with an owner has its
    // stream opened on the device chosen, for the same player and source, so
    // the call goes on through it; every player is moved to the new speaker,
    // so that a stream handed back goes there too. A null device leaves that
    // side alone (another module has it). What has no owner opens on the new
    // devices when it next starts, the configuration having them. The same
    // device chosen again is looked at afresh: a stream that serves it as it
    // is costs nothing (the bridge takes it over), one on the default in its
    // place while it was not there is opened on it now that it is back, one
    // on an endpoint that is gone is opened again, and a side whose start had
    // failed tries again. A device that will not start leaves the call on
    // the one it had (the players go back to it, so that the device that
    // failed is tried again at the next call, not at every hand-back), or on
    // the default; a microphone that cannot be had at all is replaced by
    // silence. What came of each side is returned.
    Switch switch_devices(const char *microphone_device, const char *speaker_device) {
        Switch outcome;
        if (speaker_device) {
            const std::string previous = active_playout ? active_playout->device() : "";
            for (Play *p : players) p->set_device(speaker_device);
            if (active_playout) outcome.speaker = reopen_playout(previous);
        }
        if (microphone_device && active_source) {
            const std::string previous = source_device;
            source_device = microphone_device;
            outcome.microphone = reopen_source(previous);
        }
        keep_watching();
        return outcome;
    }
    // Timer::Watch fired (kWatchMs after the last look) while a side has an
    // owner: a stream the owner started that is not running any more
    // (WebRTC's own restart of it, after the device in use went, failed;
    // nothing comes through it, and the owner does not know) is opened again
    // once it has been found so at two looks in a row: on the device asked
    // for if it is there, on the default, or silence stands in.
    void watch_tick() {
        if (active_playout) {
            if (!stream_stopped(true)) {
                playout_stopped_seen = false;
            } else if (!playout_stopped_seen) {
                playout_stopped_seen = true;
            } else {
                hooks.log("ksip_audio: the call's speaker stream stopped (WebRTC could not restart it), the stream is opened again\n");
                reopen_playout(active_playout->device());
            }
        }
        if (active_source) {
            if (!stream_stopped(false)) {
                source_stopped_seen = false;
            } else if (!source_stopped_seen) {
                source_stopped_seen = true;
            } else {
                hooks.log("ksip_audio: the call's microphone stream stopped (WebRTC could not restart it), the stream is opened again\n");
                reopen_source(source_device);
            }
        }
        keep_watching();
    }
    [[nodiscard]] Play *playout() const { return active_playout; }
    [[nodiscard]] Source *source() const { return active_source; }
    [[nodiscard]] size_t player_count() const { return players.size(); }
    // How things stand, read off who has the streams, so that it cannot
    // disagree with them: a source that started has the device, one that did
    // not runs on the fallback's silence, and a new source replaces either.
    [[nodiscard]] Input input() const { return !active_source ? Input::None : active_source->started ? Input::Device : Input::Silence; }
    // A player has the stream and it is up (an owner whose start failed is not playing).
    [[nodiscard]] bool playing() const { return active_playout && active_playout->started; }
    // The endpoint the side's call stream is on, as the bridge opened it;
    // empty while the stream is not up.
    std::string endpoint(bool playout) {
        const bool up = playout ? playing() : input() == Input::Device;
        return up ? adm.opened(playout) : std::string();
    }
    // The stream is up on another endpoint than the device asked for: the
    // default in its place. Never for "default", whatever it stands for.
    bool stand_in(bool playout) {
        const std::string wanted = asked(playout);
        if (wanted == "default") return false;
        const std::string on = endpoint(playout);
        return !on.empty() && on != wanted;
    }
    // The device starts that failed on one side: how many, and the bridge's
    // result for the last of them. Each side is kept on its own, so that a
    // failure of the speaker is not hidden by a later one of the microphone,
    // and counted, so that one between two reports is still seen.
    struct Failures {
        unsigned count = 0;
        int last_result = 0;
    };
    [[nodiscard]] Failures speaker_failures() const { return speaker; }
    [[nodiscard]] Failures microphone_failures() const { return microphone; }

private:
    // The devices one operation tried and found would not start, so that
    // the default is not tried twice in it.
    using Tried = std::vector<std::string>;
    // Whether the device a side goes to when `wanted` would not start is
    // worth trying: `back` is the one the stream had (when it had one and it
    // is another device) and then the default (when `wanted` is not it). For
    // the default asked for, also the endpoint the stream was on (`before`,
    // by its own id): the default moved to an endpoint that would not start
    // keeps the call where it was, the default staying asked for.
    static std::vector<std::string> fallbacks(const std::string &wanted, const std::string &previous, const std::string &before = "") {
        std::vector<std::string> list;
        if (!previous.empty() && previous != wanted) list.push_back(previous);
        if (wanted != "default" && previous != "default") list.push_back("default");
        if (wanted == "default" && !before.empty() && !among(list, before)) list.push_back(before);
        return list;
    }
    static bool among(const Tried &tried, const std::string &device) { return std::find(tried.begin(), tried.end(), device) != tried.end(); }
    // One start on one device for the player, failures logged and counted;
    // a device this operation already found would not start is not asked again.
    bool start_playout_on(Play *p, const std::string &device, Tried &tried) {
        if (among(tried, device)) return false;
        const int result = adm.start_playout(device.c_str(), p);
        if (result == 0) {
            p->started = true;
            return true;
        }
        failed(true, device.c_str(), result);
        p->started = false;
        tried.push_back(device);
        return false;
    }
    bool start_source_on(Source *s, const std::string &device, Tried &tried) {
        if (among(tried, device)) return false;
        const int result = adm.start_recording(device.c_str(), s);
        if (result == 0) {
            s->started = true;
            return true;
        }
        failed(false, device.c_str(), result);
        s->started = false;
        tried.push_back(device);
        return false;
    }
    // The player's stream on the device asked for, or on the default in its
    // place when that would not start, which is said.
    bool open_playout(Play *p, const std::string &wanted, Tried &tried) {
        if (start_playout_on(p, wanted, tried)) return true;
        for (const std::string &back : fallbacks(wanted, "")) {
            if (start_playout_on(p, back, tried)) {
                hooks.log("ksip_audio: the speaker would not start, the default plays in its place\n");
                return true;
            }
        }
        return false;
    }
    bool open_source(Source *s, const std::string &wanted, Tried &tried) {
        if (start_source_on(s, wanted, tried)) return true;
        for (const std::string &back : fallbacks(wanted, "")) {
            if (start_source_on(s, back, tried)) {
                hooks.log("ksip_audio: the microphone would not start, the default records in its place\n");
                return true;
            }
        }
        return false;
    }
    // The owner's stream opened on the device the players ask for now:
    // taken as it is when it serves that (the bridge's own test), opened
    // anew otherwise. When the device would not start: the device the stream
    // had, `previous`, and the players go back to it; or the default.
    Outcome reopen_playout(const std::string &previous) {
        Play *p = active_playout;
        const std::string wanted = p->device();
        const bool was_up = p->started;
        const std::string before = adm.opened(true);
        playout_stopped_seen = false;
        Tried tried;
        if (start_playout_on(p, wanted, tried)) {
            const bool moved = !was_up || adm.opened(true) != before;
            if (moved) hooks.log("ksip_audio: the call's speaker switched\n");
            return moved ? Outcome::Moved : Outcome::Unchanged;
        }
        for (const std::string &back : fallbacks(wanted, was_up ? previous : "", before)) {
            if (!start_playout_on(p, back, tried)) continue;
            if (back == previous) {
                for (Play *q : players) q->set_device(previous.c_str());
                hooks.log("ksip_audio: the call stays on the speaker it had\n");
            } else if (wanted == "default") {
                hooks.log("ksip_audio: the default speaker now would not start, the call stays on the speaker it had\n");
            } else {
                hooks.log("ksip_audio: the call's speaker is the default, in place of the one that would not start\n");
            }
            return Outcome::Kept;
        }
        return Outcome::Down;
    }
    Outcome reopen_source(const std::string &previous) {
        Source *s = active_source;
        const std::string wanted = source_device;
        const bool was_up = s->started;
        const std::string before = adm.opened(false);
        source_stopped_seen = false;
        // A source on silence tries the device again too.
        if (!was_up) hooks.stop_fallback(s);
        Tried tried;
        if (start_source_on(s, wanted, tried)) {
            const bool moved = !was_up || adm.opened(false) != before;
            if (moved) hooks.log("ksip_audio: the call's microphone switched\n");
            return moved ? Outcome::Moved : Outcome::Unchanged;
        }
        for (const std::string &back : fallbacks(wanted, was_up ? previous : "", before)) {
            if (!start_source_on(s, back, tried)) continue;
            if (back == previous) {
                source_device = previous;
                hooks.log("ksip_audio: the call stays on the microphone it had\n");
            } else if (wanted == "default") {
                hooks.log("ksip_audio: the default microphone now would not start, the call stays on the microphone it had\n");
            } else {
                hooks.log("ksip_audio: the call's microphone is the default, in place of the one that would not start\n");
            }
            return Outcome::Kept;
        }
        hooks.start_fallback(s);
        return Outcome::Down;
    }
    // The device a side asks for: the owner's.
    [[nodiscard]] std::string asked(bool playout) const { return playout ? std::string(active_playout ? active_playout->device() : "") : source_device; }
    [[nodiscard]] bool up(bool playout) const { return playout ? playing() : input() == Input::Device; }
    // The side's stream the owner started is not running: WebRTC's own
    // restart of it (the device in use went) failed, and nothing comes
    // through it, though the endpoint it opened for that is still noted.
    bool stream_stopped(bool playout) { return up(playout) && !(playout ? adm.playout_running() : adm.recording_running()); }
    // The watch runs while a side has an owner.
    void keep_watching() {
        if (!active_playout && !active_source) {
            clock.cancel(Timer::Watch);
            return;
        }
        clock.start(Timer::Watch, kWatchMs);
    }
    void keep_warm() { clock.start(Timer::Linger, kLingerMs); }
    // Every start that fails comes here, whichever path tried it: a new
    // player or source, the old player put back after a new one failed, the
    // stream handed back to the player left, the default in a device's place.
    // Logged and counted in one place, so that no path is left out of either.
    void failed(bool playout, const char *device, int result) {
        hooks.start_failed(playout, device, result);
        Failures &side = playout ? speaker : microphone;
        ++side.count;
        side.last_result = result;
    }
    Failures speaker, microphone;
    Adm &adm;
    Clock &clock;
    Hooks hooks;
    // Every player alive, oldest first. Only one at a time gets the stream, the
    // newest; when it goes, the one before it gets the stream back. That is how
    // the ringback tone of a call that is still ringing comes back when the call
    // it was pushed aside by (one the person went back to for a moment) is put on
    // hold again.
    std::vector<Play *> players;
    Play *active_playout = nullptr;
    Source *active_source = nullptr;
    // The microphone the current source asked for, so that a switch knows
    // whether it changes and where to go back to.
    std::string source_device;
    // The side's stream was found stopped at the last look (watch_tick).
    bool playout_stopped_seen = false, source_stopped_seen = false;
};
} // namespace playback_session
