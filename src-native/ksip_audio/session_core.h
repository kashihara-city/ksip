// The decisions of the audio session, with nothing real behind them: who
// has the playout stream and the microphone, what happens when a player or
// a source goes, when a start fails, and when the streams are left idle. The
// device bridge, the timers and the fallback thread are asked for through
// the interfaces below, so that the module can put the WebRTC bridge and
// libre's timers behind them, and a test can put fakes.
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
};
enum class Timer { Linger, HandBack };
// Where the current call's microphone comes from: no source, the device, or
// the timed silence that stands in for a device that would not start.
enum class Input { None, Device, Silence };
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

// `Play` has `bool started` and `const char *device()`; `Source` has `bool
// started`. The module's auplay_st and ausrc_st do; a test's records do too.
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

    // A new player takes the stream from whoever has it. Returns 0, or
    // ENODEV when the device would not start; then the caller frees the
    // player, and the one that was detached has its stream back if it could
    // be restarted, so that what was playing goes on instead of falling silent.
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
        const bool running = adm.playout_running();
        const int result = adm.start_playout(fresh->device(), fresh);
        if (result) {
            failed(true, fresh->device(), result);
            if (previous) {
                const int restored = adm.start_playout(previous->device(), previous);
                if (restored == 0) {
                    previous->started = true;
                    active_playout = previous;
                    hooks.log("ksip_audio: WebRTC ADM playout handed back after a failed start\n");
                    return ENODEV;
                }
                failed(true, previous->device(), restored);
            }
            keep_warm();
            return ENODEV;
        }
        players.push_back(fresh);
        fresh->started = true;
        active_playout = fresh;
        hooks.log(running && adm.playout_running() ? "ksip_audio: WebRTC ADM playout taken over\n" : "ksip_audio: WebRTC ADM playout started\n");
        return 0;
    }
    // A player goes. If it had the stream, the stream is detached and handed
    // back once the event that took the player away has been dealt with: a
    // tone the menu stops in the same breath must not get the stream for an
    // instant on its way out.
    void playout_gone(Play *gone) {
        players.erase(std::remove(players.begin(), players.end(), gone), players.end());
        if (gone != active_playout) return;
        active_playout = nullptr;
        if (gone->started) adm.detach_playout();
        clock.start(Timer::HandBack, 0);
    }
    // Timer::HandBack fired: the newest player left gets the stream back.
    void hand_back() {
        if (active_playout) return;
        if (!players.empty()) {
            Play *previous = players.back();
            const int result = adm.start_playout(previous->device(), previous);
            if (result == 0) {
                previous->started = true;
                active_playout = previous;
                hooks.log("ksip_audio: WebRTC ADM playout handed back\n");
                return;
            }
            failed(true, previous->device(), result);
        }
        keep_warm();
    }
    // Timer::Linger fired: streams nobody uses are closed.
    void stop_idle() {
        if (!active_playout && adm.playout_running()) adm.stop_playout();
        if (!active_source && adm.recording_running()) adm.stop_recording();
        hooks.log("ksip_audio: idle audio streams closed\n");
    }
    // The microphone is opened as soon as the speaker is, that is while the
    // ringback tone plays, so that it is up by the time the call is answered.
    // What it captures until then is dropped.
    void open_microphone_ahead(const char *device) {
        if (active_source || adm.recording_running()) return;
        if (adm.start_recording(device, nullptr) == 0) hooks.log("ksip_audio: microphone opened ahead of the call\n");
    }
    // A new source takes the microphone from whoever has it. A device that
    // will not start is replaced by the fallback; the call goes on either way.
    int take_source(Source *fresh, const char *device) {
        clock.cancel(Timer::Linger);
        if (active_source) {
            adm.detach_recording();
            active_source->started = false;
            hooks.stop_fallback(active_source);
            active_source = nullptr;
        }
        const bool running = adm.recording_running();
        const int result = adm.start_recording(device, fresh);
        if (result) {
            failed(false, device, result);
            hooks.start_fallback(fresh);
        } else {
            fresh->started = true;
            hooks.log(running && adm.recording_running() ? "ksip_audio: WebRTC ADM recording taken over, APM running\n"
                                                        : "ksip_audio: WebRTC ADM recording and APM started\n");
        }
        active_source = fresh;
        return 0;
    }
    // A source goes; its fallback is the caller's to stop first.
    void source_gone(Source *gone) {
        if (gone != active_source) return;
        if (gone->started) adm.detach_recording();
        active_source = nullptr;
        keep_warm();
    }
    Play *playout() const { return active_playout; }
    Source *source() const { return active_source; }
    size_t player_count() const { return players.size(); }
    // How things stand, read off who has the streams, so that it cannot
    // disagree with them: a source that started has the device, one that did
    // not runs on the fallback's silence, and a new source replaces either.
    Input input() const { return !active_source ? Input::None : active_source->started ? Input::Device : Input::Silence; }
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
    void keep_warm() { clock.start(Timer::Linger, kLingerMs); }
    // Every start that fails comes here, whichever path tried it: a new
    // player or source, the old player put back after a new one failed, the
    // stream handed back to the player left. Logged and counted in one place,
    // so that no path is left out of either.
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
};
} // namespace playback_session
