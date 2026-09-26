// Who has the speaker and the microphone; see audio_session.h.
#include "audio_session.h"
#include <cerrno>
#include <chrono>
#include <cstring>
#include <new>
#include <vector>

namespace playback_session {
namespace {
// How long the streams stay open after their last user has gone. Opening a
// device takes a second or more on some hardware, and a call waits for it: the
// ACK goes out once the audio is up. The ringback tone hands its stream to the
// call, and a call that ends hands its streams to the next one within this
// time; after it the devices are closed, so nothing is held while idle.
constexpr uint64_t kLingerMs = 3000;
ksip_audio *g_audio;
auplay_st *g_active_playout;
ausrc_st *g_active_source;
struct tmr g_linger;
struct tmr g_handback;
// Every player alive, oldest first. Only one at a time gets the stream, the
// newest; when it goes, the one before it gets the stream back. That is how
// the ringback tone of a call that is still ringing comes back when the call
// it was pushed aside by (one the person went back to for a moment) is put on
// hold again.
std::vector<auplay_st *> g_players;

int Render(void *arg, int16_t *samples, size_t frames) {
    auto *state = static_cast<auplay_st *>(arg);
    struct auframe frame;
    auframe_init(&frame, AUFMT_S16LE, samples, frames, kRate, kChannels);
    state->handler(&frame, state->arg);
    return 0;
}
void Capture(void *arg, const int16_t *samples, size_t frames, int64_t time_ns) {
    auto *state = static_cast<ausrc_st *>(arg);
    struct auframe frame;
    auframe_init(&frame, AUFMT_S16LE, const_cast<int16_t *>(samples), frames, kRate, kChannels);
    frame.timestamp = time_ns > 0 ? static_cast<uint64_t>(time_ns / 1000) : tmr_jiffies_usec();
    state->handler(&frame, state->arg);
}
void StopFallback(ausrc_st *state) {
    state->fallback_run.store(false);
    if (state->fallback_thread.joinable()) state->fallback_thread.join();
}
// Frames captured ahead of a call go nowhere.
void Discard(void *, const int16_t *, size_t, int64_t) {}
void StopIdleStreams(void *) {
    if (!g_audio) return;
    if (!g_active_playout && ksip_audio_playout_running(g_audio)) ksip_audio_stop_playout(g_audio);
    if (!g_active_source && ksip_audio_recording_running(g_audio)) ksip_audio_stop_recording(g_audio);
    info("ksip_audio: idle audio streams closed\n");
}
void KeepWarm() { tmr_start(&g_linger, kLingerMs, StopIdleStreams, nullptr); }
// Runs once the event that took the active player away has been dealt with:
// a tone the menu stops in the same breath must not get the stream for an
// instant on its way out.
void HandBack(void *) {
    if (!g_audio || g_active_playout) return;
    if (!g_players.empty()) {
        auto *previous = g_players.back();
        if (ksip_audio_start_playout(g_audio, previous->device, Render, previous) == 0) {
            previous->started = true;
            g_active_playout = previous;
            info("ksip_audio: WebRTC ADM playout handed back\n");
            return;
        }
    }
    KeepWarm();
}
void PlayoutDestructor(void *arg) {
    auto *state = static_cast<auplay_st *>(arg);
    g_players.erase(std::remove(g_players.begin(), g_players.end(), state), g_players.end());
    if (state != g_active_playout) return;
    g_active_playout = nullptr;
    if (!g_audio) return;
    if (state->started) ksip_audio_detach_playout(g_audio);
    tmr_start(&g_handback, 0, HandBack, nullptr);
}
void SourceDestructor(void *arg) {
    auto *state = static_cast<ausrc_st *>(arg);
    StopFallback(state);
    if (state == g_active_source) {
        if (state->started && g_audio) ksip_audio_detach_recording(g_audio);
        g_active_source = nullptr;
        KeepWarm();
    }
    state->fallback_thread.~thread();
    state->fallback_run.~atomic();
}
// The microphone is opened as soon as the speaker is, that is while the
// ringback tone plays, so that it is up by the time the call is answered.
// What it captures until then is dropped.
void OpenMicrophoneAhead() {
    if (g_active_source || ksip_audio_recording_running(g_audio)) return;
    const struct config *cfg = conf_config();
    if (!cfg || str_cmp(cfg->audio.src_mod, "ksip_audio")) return;
    const char *device = cfg->audio.src_dev[0] ? cfg->audio.src_dev : "default";
    if (ksip_audio_start_recording(g_audio, device, Discard, nullptr) == 0) info("ksip_audio: microphone opened ahead of the call\n");
}
} // namespace

void open(ksip_audio *bridge) {
    g_audio = bridge;
    tmr_init(&g_linger);
    tmr_init(&g_handback);
}
void close() {
    tmr_cancel(&g_linger);
    tmr_cancel(&g_handback);
    g_audio = nullptr;
}
ksip_audio *bridge() { return g_audio; }

int allocate_playout(auplay_st **out, const char *device, auplay_write_h *handler, void *arg) {
    tmr_cancel(&g_linger);
    tmr_cancel(&g_handback);
    auplay_st *previous = nullptr;
    if (g_active_playout) {
        ksip_audio_detach_playout(g_audio);
        g_active_playout->started = false;
        previous = g_active_playout;
        g_active_playout = nullptr;
    }
    auto *state = static_cast<auplay_st *>(mem_zalloc(sizeof(auplay_st), PlayoutDestructor));
    if (!state) return ENOMEM;
    state->handler = handler;
    state->arg = arg;
    str_ncpy(state->device, device && device[0] ? device : "default", sizeof(state->device));
    const bool running = ksip_audio_playout_running(g_audio);
    const int result = ksip_audio_start_playout(g_audio, state->device, Render, state);
    if (result) {
        warning("ksip_audio: start playout failed (%d) for %s\n", result, state->device);
        mem_deref(state);
        // The player that was detached for a start that did not happen takes its
        // stream back, so what was playing goes on instead of falling silent.
        // Failing that too, the idle streams close in their own time.
        if (previous && ksip_audio_start_playout(g_audio, previous->device, Render, previous) == 0) {
            previous->started = true;
            g_active_playout = previous;
            info("ksip_audio: WebRTC ADM playout handed back after a failed start\n");
        } else {
            KeepWarm();
        }
        return ENODEV;
    }
    g_players.push_back(state);
    state->started = true;
    g_active_playout = state;
    *out = state;
    info(running && ksip_audio_playout_running(g_audio) ? "ksip_audio: WebRTC ADM playout taken over\n" : "ksip_audio: WebRTC ADM playout started\n");
    OpenMicrophoneAhead();
    return 0;
}
int allocate_source(ausrc_st **out, const char *device, ausrc_read_h *handler, void *arg) {
    tmr_cancel(&g_linger);
    if (g_active_source) {
        ksip_audio_detach_recording(g_audio);
        g_active_source->started = false;
        StopFallback(g_active_source);
        g_active_source = nullptr;
    }
    auto *state = static_cast<ausrc_st *>(mem_zalloc(sizeof(ausrc_st), SourceDestructor));
    if (!state) return ENOMEM;
    new (&state->fallback_run) std::atomic<bool>(false);
    new (&state->fallback_thread) std::thread();
    state->handler = handler;
    state->arg = arg;
    const bool running = ksip_audio_recording_running(g_audio);
    const char *wanted = device && device[0] ? device : "default";
    const int result = ksip_audio_start_recording(g_audio, wanted, Capture, state);
    if (result) {
        warning("ksip_audio: start recording failed (%d) for %s; using silence\n", result, wanted);
        state->fallback_run.store(true);
        state->fallback_thread = std::thread([state] {
            int16_t samples[kFrames] = {};
            info("ksip: microphone fallback active\n");
            auto next = std::chrono::steady_clock::now();
            while (state->fallback_run.load()) {
                Capture(state, samples, kFrames, 0);
                next += std::chrono::milliseconds(10);
                std::this_thread::sleep_until(next);
            }
        });
    } else {
        state->started = true;
        info(running && ksip_audio_recording_running(g_audio) ? "ksip_audio: WebRTC ADM recording taken over, APM running\n"
                                                              : "ksip_audio: WebRTC ADM recording and APM started\n");
    }
    g_active_source = state;
    *out = state;
    return 0;
}
} // namespace playback_session
