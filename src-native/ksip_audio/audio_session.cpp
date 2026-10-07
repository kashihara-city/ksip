// Who has the speaker and the microphone; see audio_session.h. The decisions
// are the session core's (session_core.h); this file puts the WebRTC bridge,
// libre's timers and the fallback thread behind the core's interfaces, and
// turns the bridge's PCM callbacks into baresip frames.
#include "audio_session.h"
#include "libre_clock.h"
#include "session_core.h"
#include "ksip_audio_bridge.h"
#include <cerrno>
#include <chrono>
#include <cstring>
#include <atomic>
#include <cstdint>
#include <string>
#include <thread>
#include <baresip.h>

namespace playback_session {
namespace {
ksip_audio *g_audio;
// A frame of the fallback source is 10 ms, as the bridge's are; and the
// clock's units.
constexpr int kFrameMs = 10;
constexpr int64_t kNsPerUs = 1000;

int Render(void *arg, int16_t *samples, size_t frames) {
    const auto *state = static_cast<auplay_st *>(arg);
    struct auframe frame;
    auframe_init(&frame, AUFMT_S16LE, samples, frames, kRate, kChannels);
    state->handler(&frame, state->arg);
    return 0;
}
void Capture(void *arg, const int16_t *samples, size_t frames, int64_t time_ns) {
    const auto *state = static_cast<ausrc_st *>(arg);
    struct auframe frame;
    auframe_init(&frame, AUFMT_S16LE, const_cast<int16_t *>(samples), frames, kRate, kChannels);
    frame.timestamp = time_ns > 0 ? static_cast<uint64_t>(time_ns / kNsPerUs) : tmr_jiffies_usec();
    state->handler(&frame, state->arg);
}
// Frames captured ahead of a call go nowhere.
void Discard(void *, const int16_t *, size_t, int64_t) {}

// The bridge, as the core sees it.
struct BridgeAdm final : Adm {
    int start_playout(const char *device, void *player) override { return ksip_audio_start_playout(g_audio, device, Render, player); }
    void detach_playout() override { ksip_audio_detach_playout(g_audio); }
    bool playout_running() override { return ksip_audio_playout_running(g_audio) != 0; }
    void stop_playout() override { ksip_audio_stop_playout(g_audio); }
    int start_recording(const char *device, void *source) override {
        return source ? ksip_audio_start_recording(g_audio, device, Capture, source) : ksip_audio_start_recording(g_audio, device, Discard, nullptr);
    }
    void detach_recording() override { ksip_audio_detach_recording(g_audio); }
    bool recording_running() override { return ksip_audio_recording_running(g_audio) != 0; }
    void stop_recording() override { ksip_audio_stop_recording(g_audio); }
    std::string opened(bool playout) override {
        char id[KSIP_AUDIO_DEVICE_TEXT_SIZE] = {};
        ksip_audio_opened_endpoint(g_audio, static_cast<int>(playout), id, sizeof id);
        return id;
    }
} g_adm;
LibreClock g_clock;

void StopFallback(ausrc_st *state) {
    state->fallback_run.store(false);
    if (state->fallback_thread.joinable()) state->fallback_thread.join();
}
void StartFallback(ausrc_st *state) {
    state->fallback_run.store(true);
    state->fallback_thread = std::thread([state] {
        int16_t samples[kFrames] = {};
        info("ksip: microphone fallback active\n");
        auto next = std::chrono::steady_clock::now();
        while (state->fallback_run.load()) {
            Capture(state, samples, kFrames, 0);
            next += std::chrono::milliseconds(kFrameMs);
            std::this_thread::sleep_until(next);
        }
    });
}
Core<auplay_st, ausrc_st> g_core(g_adm, g_clock,
                                 {[](const char *line) { info("%s", line); },
                                  [](bool playout, const char *device, int result) {
                                      if (playout) warning("ksip_audio: start playout failed (%d) for %s\n", result, device);
                                      else warning("ksip_audio: start recording failed (%d) for %s; using silence\n", result, device);
                                  },
                                  StartFallback, StopFallback});

void PlayoutDestructor(void *arg) {
    auto *state = static_cast<auplay_st *>(arg);
    if (!g_audio) return;
    g_core.playout_gone(state);
}
void SourceDestructor(void *arg) {
    auto *state = static_cast<ausrc_st *>(arg);
    StopFallback(state);
    if (g_audio) g_core.source_gone(state);
    state->fallback_thread.~thread();
    state->fallback_run.~atomic();
}
// The microphone the config names, when this module is the source.
const char *ConfiguredMicrophone() {
    const struct config *cfg = conf_config();
    if (!cfg || str_cmp(cfg->audio.src_mod, "ksip_audio")) return nullptr;
    return cfg->audio.src_dev[0] ? cfg->audio.src_dev : "default";
}
} // namespace

void open(ksip_audio *bridge) {
    g_audio = bridge;
    g_clock.fire = [](Timer which) {
        if (!g_audio) return;
        if (which == Timer::Linger) g_core.stop_idle();
        else if (which == Timer::HandBack) g_core.hand_back();
        else g_core.watch_tick();
    };
}
void close() {
    g_clock.cancel_all();
    g_audio = nullptr;
}
ksip_audio *bridge() { return g_audio; }
Switch switch_devices(const char *microphone, const char *speaker) {
    if (!g_audio) return Switch{};
    return g_core.switch_devices(microphone, speaker);
}
namespace {
void AddFailures(odict *side, const Core<auplay_st, ausrc_st>::Failures &failures) {
    odict_entry_add(side, "failures", ODICT_INT, static_cast<int64_t>(failures.count));
    if (failures.count) odict_entry_add(side, "last_result", ODICT_INT, static_cast<int64_t>(failures.last_result));
}
// The endpoint the side's call stream is on, while it is up, and whether it
// stands in for the device asked for.
void AddEndpoint(odict *side, bool playout) {
    const std::string endpoint = g_core.endpoint(playout);
    if (endpoint.empty()) return;
    odict_entry_add(side, "endpoint", ODICT_STRING, endpoint.c_str());
    odict_entry_add(side, "stand_in", ODICT_BOOL, static_cast<int>(g_core.stand_in(playout)));
}
} // namespace
void add_state(odict *audio) {
    static const char *const inputs[] = {"none", "device", "silence"};
    odict *microphone = nullptr;
    if (!odict_alloc(&microphone, kDictBuckets)) {
        odict_entry_add(microphone, "input", ODICT_STRING, inputs[static_cast<int>(g_core.input())]);
        AddEndpoint(microphone, false);
        AddFailures(microphone, g_core.microphone_failures());
        odict_entry_add(audio, "microphone", ODICT_OBJECT, microphone);
    }
    mem_deref(microphone);
    add_playout_state(audio, "speaker", g_core);
}

int allocate_playout(auplay_st **out, const char *device, auplay_write_h *handler, void *arg) {
    auto *state = static_cast<auplay_st *>(mem_zalloc(sizeof(auplay_st), PlayoutDestructor));
    if (!state) return ENOMEM;
    state->handler = handler;
    state->arg = arg;
    str_ncpy(state->device_name, device && device[0] ? device : "default", sizeof(state->device_name));
    const int result = g_core.take_playout(state);
    if (result) {
        // Never the core's player (its start did not happen): freed here
        // without the destructor telling the core anything.
        mem_destructor(state, nullptr);
        mem_deref(state);
        return result;
    }
    *out = state;
    if (auto microphone = ConfiguredMicrophone()) g_core.open_microphone_ahead(microphone);
    return 0;
}
int allocate_source(ausrc_st **out, const char *device, ausrc_read_h *handler, void *arg) {
    auto *state = static_cast<ausrc_st *>(mem_zalloc(sizeof(ausrc_st), SourceDestructor));
    if (!state) return ENOMEM;
    new (&state->fallback_run) std::atomic<bool>(false);
    new (&state->fallback_thread) std::thread();
    state->handler = handler;
    state->arg = arg;
    g_core.take_source(state, device && device[0] ? device : "default");
    *out = state;
    return 0;
}
} // namespace playback_session
