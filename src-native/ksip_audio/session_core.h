// The decisions of the audio session, with nothing real behind them: who
// has the playout stream and the microphone, what happens when a player or
// a source goes, when a start fails, when a device that was not there is
// back, and when the streams are left idle. The device bridge, the timers
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
    // Whether the side could open the device now: it is among the endpoints
    // listed for the side; "default" is while the side has any endpoint at all.
    virtual bool listed(const char *device, bool playout) = 0;
};
enum class Timer { Linger, HandBack, Watch };
// Where the current call's microphone comes from: no source, the device, or
// the timed silence that stands in for a device that would not start.
enum class Input { None, Device, Silence };
// How one side came out of a switch of devices, or of a return to a device
// that is back (switch_devices, watch_tick).
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
// How often a side with an owner looks whether its stream is still on the
// device asked for and that device still there: once a second while every
// side is, and every quarter second while one is not (the default stands in,
// or nothing does), so that a device back is taken within half a second.
constexpr uint64_t kWatchMs = 1000;
constexpr uint64_t kWatchFastMs = 250;
// How long a device that is back has to have been there, over at least two
// looks, before the stream is opened on it again: one that comes and goes
// (a hub without power, a headset reconnecting) does not have the call go
// back and forth.
constexpr uint64_t kBackSettleMs = 250;
// How long the endpoint a stream is up on has to have been gone before the
// stream is opened again from here: WebRTC's own move to the default, on its
// audio thread, is over by then, and so is the moment of a device change in
// which Windows lists no endpoint at all.
constexpr uint64_t kGoneSettleMs = 1000;

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
        playout_watch = Watch{};
        const bool running = adm.playout_running();
        Tried tried;
        bool fell_back = false;
        if (open_playout(fresh, fresh->device(), tried, fell_back)) {
            players.push_back(fresh);
            active_playout = fresh;
            hooks.log(running && adm.playout_running() ? "ksip_audio: WebRTC ADM playout taken over\n" : "ksip_audio: WebRTC ADM playout started\n");
            note_opened(true, fresh->device(), fell_back);
            keep_watching();
            return 0;
        }
        if (previous) {
            // The owner either way, so that a later try (another device
            // chosen, the device back) can start it.
            active_playout = previous;
            if (open_playout(previous, previous->device(), tried, fell_back)) {
                hooks.log("ksip_audio: WebRTC ADM playout handed back after a failed start\n");
                note_opened(true, previous->device(), fell_back);
                keep_watching();
                return ENODEV;
            }
            note_down(true, previous->device());
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
        playout_watch = Watch{};
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
            bool fell_back = false;
            if (open_playout(previous, previous->device(), tried, fell_back)) {
                hooks.log("ksip_audio: WebRTC ADM playout handed back\n");
                note_opened(true, previous->device(), fell_back);
                keep_watching();
                return;
            }
            note_down(true, previous->device());
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
        source_watch = Watch{};
        source_device = device;
        const bool running = adm.recording_running();
        active_source = fresh;
        Tried tried;
        bool fell_back = false;
        if (open_source(fresh, source_device, tried, fell_back)) {
            hooks.log(running && adm.recording_running() ? "ksip_audio: WebRTC ADM recording taken over, APM running\n"
                                                        : "ksip_audio: WebRTC ADM recording and APM started\n");
            note_opened(false, source_device, fell_back);
        } else {
            hooks.start_fallback(fresh);
            note_down(false, source_device);
        }
        keep_watching();
        return 0;
    }
    // A source goes; its fallback is the caller's to stop first.
    void source_gone(Source *gone) {
        if (gone != active_source) return;
        if (gone->started) adm.detach_recording();
        active_source = nullptr;
        source_watch = Watch{};
        keep_warm();
        keep_watching();
    }
    // The person chose devices while a call is up: each side with an owner
    // has its stream opened on the device chosen, for the same player and
    // source, so the call goes on through it; every player is moved to the
    // new speaker, so that a stream handed back goes there too. A null device
    // leaves that side alone (another module has it). What has no owner opens
    // on the new devices when it next starts, the configuration having them.
    // The same device chosen again is looked at afresh: a stream that serves
    // it as it is costs nothing (the bridge takes it over), one on the default
    // in its place while it was not there is opened on it now that it is
    // back, and a side whose start had failed tries again. A device that will
    // not start leaves the call on the one it had (the players go back to it,
    // so that the device that failed is tried again at the next call, not at
    // every hand-back), or on the default; a microphone that cannot be had at
    // all is replaced by silence. What came of each side is returned.
    Switch switch_devices(const char *microphone, const char *speaker) {
        Switch outcome;
        if (speaker) {
            const std::string previous = active_playout ? active_playout->device() : "";
            for (Play *p : players) p->set_device(speaker);
            if (active_playout) outcome.speaker = reopen_playout(previous);
        }
        if (microphone && active_source) {
            const std::string previous = source_device;
            source_device = microphone;
            outcome.microphone = reopen_source(previous);
        }
        keep_watching();
        return outcome;
    }
    // Timer::Watch fired (kWatchMs or kWatchFastMs after the last look) while
    // a side has an owner. A stream up on an endpoint that is gone (unplugged
    // under the call, and WebRTC did not move it, which it cannot with no
    // other device to move to; it then runs on, delivering nothing) is opened
    // again: on the device asked for if it is there, on the default, or
    // silence stands in. Only once the endpoint has been gone for
    // kGoneSettleMs: WebRTC's own move to the default is not to be interfered
    // with from here. A side whose stream is not on the device asked for (the
    // default stands in: the device was not there, or went away, or would not
    // start) looks whether the device is there now, every kWatchFastMs. On
    // the device being back, there for kBackSettleMs over at least two looks,
    // the stream is opened on it again, once: a return that fails leaves the
    // stream where it was until the device has gone and come back again, so a
    // device that is there but cannot be opened is not tried every look. For
    // the default asked for, "back" is any device at all being there again
    // (Adm::listed); which device the default is, is left to WebRTC.
    void watch_tick() {
        const uint64_t elapsed = watch_interval;
        bool waiting = false;
        if (active_playout) waiting |= watch(true, elapsed);
        if (active_source) waiting |= watch(false, elapsed);
        keep_watching(waiting);
    }
    Play *playout() const { return active_playout; }
    Source *source() const { return active_source; }
    size_t player_count() const { return players.size(); }
    // How things stand, read off who has the streams, so that it cannot
    // disagree with them: a source that started has the device, one that did
    // not runs on the fallback's silence, and a new source replaces either.
    Input input() const { return !active_source ? Input::None : active_source->started ? Input::Device : Input::Silence; }
    // A player has the stream and it is up (an owner whose start failed is not playing).
    bool playing() const { return active_playout && active_playout->started; }
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
    Failures speaker_failures() const { return speaker; }
    Failures microphone_failures() const { return microphone; }

private:
    // The devices one operation tried and found would not start, so that
    // the default is not tried twice in it.
    using Tried = std::vector<std::string>;
    // Whether the device a side goes to when `wanted` would not start is
    // worth trying: `back` is the one the stream had (when it had one and it
    // is another device) and then the default (when `wanted` is not it).
    static std::vector<std::string> fallbacks(const std::string &wanted, const std::string &previous) {
        std::vector<std::string> list;
        if (!previous.empty() && previous != wanted) list.push_back(previous);
        if (wanted != "default" && previous != "default") list.push_back("default");
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
    // place when that would not start, which is said and `fell_back` tells.
    bool open_playout(Play *p, const std::string &wanted, Tried &tried, bool &fell_back) {
        fell_back = false;
        if (start_playout_on(p, wanted, tried)) return true;
        for (const std::string &back : fallbacks(wanted, "")) {
            if (start_playout_on(p, back, tried)) {
                hooks.log("ksip_audio: the speaker would not start, the default plays in its place\n");
                fell_back = true;
                return true;
            }
        }
        return false;
    }
    bool open_source(Source *s, const std::string &wanted, Tried &tried, bool &fell_back) {
        fell_back = false;
        if (start_source_on(s, wanted, tried)) return true;
        for (const std::string &back : fallbacks(wanted, "")) {
            if (start_source_on(s, back, tried)) {
                hooks.log("ksip_audio: the microphone would not start, the default records in its place\n");
                fell_back = true;
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
        Tried tried;
        if (start_playout_on(p, wanted, tried)) {
            const bool moved = !was_up || adm.opened(true) != before;
            if (moved) hooks.log("ksip_audio: the call's speaker switched\n");
            note_opened(true, wanted, false);
            return moved ? Outcome::Moved : Outcome::Unchanged;
        }
        for (const std::string &back : fallbacks(wanted, was_up ? previous : "")) {
            if (!start_playout_on(p, back, tried)) continue;
            if (back == previous) {
                for (Play *q : players) q->set_device(previous.c_str());
                hooks.log("ksip_audio: the call stays on the speaker it had\n");
                note_opened(true, previous, false);
            } else {
                hooks.log("ksip_audio: the call's speaker is the default, in place of the one that would not start\n");
                note_opened(true, wanted, true);
            }
            return Outcome::Kept;
        }
        note_down(true, wanted);
        return Outcome::Down;
    }
    Outcome reopen_source(const std::string &previous) {
        Source *s = active_source;
        const std::string wanted = source_device;
        const bool was_up = s->started;
        const std::string before = adm.opened(false);
        // A source on silence tries the device again too.
        if (!was_up) hooks.stop_fallback(s);
        Tried tried;
        if (start_source_on(s, wanted, tried)) {
            const bool moved = !was_up || adm.opened(false) != before;
            if (moved) hooks.log("ksip_audio: the call's microphone switched\n");
            note_opened(false, wanted, false);
            return moved ? Outcome::Moved : Outcome::Unchanged;
        }
        for (const std::string &back : fallbacks(wanted, was_up ? previous : "")) {
            if (!start_source_on(s, back, tried)) continue;
            if (back == previous) {
                source_device = previous;
                hooks.log("ksip_audio: the call stays on the microphone it had\n");
                note_opened(false, previous, false);
            } else {
                hooks.log("ksip_audio: the call's microphone is the default, in place of the one that would not start\n");
                note_opened(false, wanted, true);
            }
            return Outcome::Kept;
        }
        hooks.start_fallback(s);
        note_down(false, wanted);
        return Outcome::Down;
    }
    // The device a side asks for: the owner's.
    std::string asked(bool playout) const { return playout ? std::string(active_playout ? active_playout->device() : "") : source_device; }
    bool up(bool playout) const { return playout ? playing() : input() == Input::Device; }
    // The side's stream is up on an endpoint that is gone: the bridge still
    // counts it as running, and nothing comes through it.
    bool on_gone_endpoint(bool playout) {
        if (!up(playout)) return false;
        const std::string on = adm.opened(playout);
        return on.empty() || !adm.listed(on.c_str(), playout);
    }
    // The side's stream is up on the device asked for, and that is there;
    // for the default asked for, up on an endpoint that is there.
    bool on_wanted(bool playout) {
        if (!up(playout) || on_gone_endpoint(playout)) return false;
        const std::string wanted = asked(playout);
        return wanted == "default" || adm.opened(playout) == wanted;
    }
    // The watch on one side: whether its stream is on the device it asks
    // for, and, while it is not, whether that device has gone and come back.
    struct Watch {
        bool active = false;        // the stream is not on the device asked for
        bool armed = false;         // the device has been seen not there since; its coming back is to be acted on
        bool seen = false;          // the device was there at the last look, once armed
        uint64_t listed_ms = 0;     // how long it has been there since, over the looks after the first
        bool gone_seen = false;     // the stream was up on an endpoint that is gone at the last look
        uint64_t gone_ms = 0;       // how long that has been so since, over the looks after the first
    };
    static Watch watching(bool armed) {
        Watch watch;
        watch.active = true;
        watch.armed = armed;
        return watch;
    }
    // A stream just opened for `wanted`: on it, or on another endpoint. The
    // bridge opens the default in place of a device that is not there, and
    // then the device coming back is to be acted on, even if it is back
    // already. The default this core fell back on (`fell_back`) for a device
    // that is there but would not start is another matter: that device is
    // tried again once it has gone and come back, not while it stays.
    void note_opened(bool playout, const std::string &wanted, bool fell_back) {
        Watch &watch = playout ? playout_watch : source_watch;
        if (on_wanted(playout)) {
            watch = Watch{};
            return;
        }
        watch = watching(!fell_back || !adm.listed(wanted.c_str(), playout));
    }
    // A side whose stream is down: watched the same, armed when the device
    // is not there (it coming back is what to act on; for the default, any
    // device at all).
    void note_down(bool playout, const std::string &wanted) {
        Watch &watch = playout ? playout_watch : source_watch;
        watch = watching(!adm.listed(wanted.c_str(), playout));
    }
    // One look at one side, `elapsed` ms after the last; whether the side
    // waits for a device (is not on the one asked for), which has the looks
    // come fast.
    bool watch(bool playout, uint64_t elapsed) {
        Watch &watch = playout ? playout_watch : source_watch;
        const std::string wanted = asked(playout);
        if (on_wanted(playout)) {
            watch = Watch{};
            return false;
        }
        if (on_gone_endpoint(playout)) {
            // Looked at again at the slow pace: WebRTC's own move is what is
            // waited for, not a device.
            if (!watch.gone_seen) {
                watch.gone_seen = true;
                watch.gone_ms = 0;
                return false;
            }
            watch.gone_ms += elapsed;
            if (watch.gone_ms < kGoneSettleMs) return false;
            watch.gone_seen = false;
            watch.gone_ms = 0;
            hooks.log(playout ? "ksip_audio: the speaker in use is gone, the call's stream is opened again\n"
                              : "ksip_audio: the microphone in use is gone, the call's stream is opened again\n");
            // On the device asked for if it is there (the bridge opens the
            // default in place of one that is not), else the default, else
            // nothing or silence; what came of it is noted for the looks to come.
            if (playout) reopen_playout(wanted);
            else reopen_source(wanted);
            return !on_wanted(playout);
        }
        watch.gone_seen = false;
        watch.gone_ms = 0;
        const bool listed = adm.listed(wanted.c_str(), playout);
        // Not on its device and not noted so: WebRTC moved the stream by
        // itself when the device went away, and it coming back is to be
        // acted on, even if it is back already.
        if (!watch.active) watch = watching(true);
        if (!listed) {
            watch.armed = true;
            watch.seen = false;
            watch.listed_ms = 0;
            return true;
        }
        if (!watch.armed) return true;
        if (!watch.seen) {
            watch.seen = true;
            watch.listed_ms = 0;
            return true;
        }
        watch.listed_ms += elapsed;
        if (watch.listed_ms < kBackSettleMs) return true;
        watch.armed = false;
        watch.seen = false;
        watch.listed_ms = 0;
        hooks.log(playout ? "ksip_audio: the speaker chosen is back, the call's stream is opened on it again\n"
                          : "ksip_audio: the microphone chosen is back, the call's stream is opened on it again\n");
        const Outcome outcome = playout ? reopen_playout(wanted) : reopen_source(wanted);
        if (outcome != Outcome::Moved) hooks.log("ksip_audio: the device that is back would not start, the call stays where it was\n");
        return !on_wanted(playout);
    }
    // The watch runs while a side has an owner: fast while a side waits for
    // a device to come back, slow otherwise.
    void keep_watching() {
        const bool waiting = (active_playout && !on_wanted(true) && !on_gone_endpoint(true)) ||
                             (active_source && !on_wanted(false) && !on_gone_endpoint(false));
        keep_watching(waiting);
    }
    void keep_watching(bool waiting) {
        if (!active_playout && !active_source) {
            clock.cancel(Timer::Watch);
            return;
        }
        watch_interval = waiting ? kWatchFastMs : kWatchMs;
        clock.start(Timer::Watch, watch_interval);
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
    Watch playout_watch, source_watch;
    // How long after the last look the next comes (the timer's interval), so
    // that a look knows how much time the device it waits for has been there.
    uint64_t watch_interval = kWatchMs;
};
} // namespace playback_session
