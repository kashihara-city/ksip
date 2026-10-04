// The alert sounds' player; see alert_player.h.
#include "audio_session.h"
#include "alert_player.h"
#include "libre_clock.h"
#include <cerrno>
#include <cstring>
#include <string>

namespace alert_player {
namespace {
using playback_session::Adm;
using playback_session::Core;
using playback_session::Outcome;
using playback_session::Timer;

// What baresip holds for a sound: the device asked for, whether the sound
// is up, and, while it is, the wasapi player doing the work, which the core
// has opened on an endpoint and may open again on another. The sound's own
// write handler is given to each wasapi player in turn.
struct Alert {
    auplay_write_h *handler;
    void *arg;
    auplay_prm prm;
    bool started;
    char device_name[160];
    struct auplay_st *inner;
    const char *device() const { return device_name; }
    void set_device(const char *device) { str_ncpy(device_name, device && device[0] ? device : "default", sizeof(device_name)); }
};
// The core has a recording side; the alert sounds have none.
struct NoSource {
    bool started = false;
};
struct auplay *g_player = nullptr;

// Whether a speaker is there, by the test the calls go by (the bridge's list
// of the endpoints WebRTC can open; "default" while there is any), so that
// the ringtone and the call are never on different speakers for it.
bool usable(const char *id) {
    ksip_audio *bridge = playback_session::bridge();
    return bridge && ksip_audio_endpoint_listed(bridge, id, 1);
}

// baresip's wasapi player, as the core sees a device bridge: one player at a
// time, opened on the endpoint the device asked for stands for (the device,
// or the default while it is not there, as device_selection.cc does for the
// calls), opened again when asked for another, and taken as it is when it
// still serves the device asked for (the bridge's Serves, in the small).
struct WasapiAdm final : Adm {
    Alert *current = nullptr;
    // The device the running player was asked for, and the endpoint it got.
    std::string request, on;
    bool serves(const std::string &asked) {
        if (!current || !current->inner || !usable(on.c_str())) return false;
        return asked == "default" || on == asked || !usable(asked.c_str());
    }
    int start_playout(const char *device, void *player) override {
        auto *p = static_cast<Alert *>(player);
        const std::string asked = device && device[0] ? device : "default";
        if (p == current && request == asked && serves(asked)) return 0;
        stop_playout();
        const bool chosen = asked != "default";
        const std::string endpoint = chosen && usable(asked.c_str()) ? asked : "default";
        if (chosen && endpoint != asked)
            warning("ksip_alert: speaker %s is not there, the alert sound plays on the default communications speaker\n", asked.c_str());
        const int err = auplay_alloc(&p->inner, baresip_auplayl(), "wasapi", &p->prm, endpoint.c_str(), p->handler, p->arg);
        if (err) return err;
        current = p;
        request = asked;
        on = endpoint;
        return 0;
    }
    // The sound's handler is the player's own: letting go of it is stopping it.
    void detach_playout() override { stop_playout(); }
    bool playout_running() override { return current && current->inner; }
    void stop_playout() override {
        if (current) current->inner = static_cast<struct auplay_st *>(mem_deref(current->inner));
        current = nullptr;
        request.clear();
        on.clear();
    }
    int start_recording(const char *, void *) override { return ENOSYS; }
    void detach_recording() override {}
    bool recording_running() override { return false; }
    void stop_recording() override {}
    std::string opened(bool playout) override { return playout ? on : std::string(); }
    bool listed(const char *device, bool) override { return usable(device); }
} g_adm;
playback_session::LibreClock g_clock;
Core<Alert, NoSource> g_core(g_adm, g_clock,
                             {[](const char *line) {
                                  // The core's words are the call's; here they are the alert's.
                                  std::string said = line;
                                  if (said.rfind("ksip_audio:", 0) == 0) said.replace(0, 10, "ksip_alert");
                                  for (const auto &[call, alert] : {std::pair{" WebRTC ADM", ""}, std::pair{"the call's", "the alert sound's"}, std::pair{"the call ", "the alert sound "}}) {
                                      for (size_t at = said.find(call); at != std::string::npos; at = said.find(call, at + std::strlen(alert)))
                                          said.replace(at, std::strlen(call), alert);
                                  }
                                  info("%s", said.c_str());
                              },
                              [](bool, const char *device, int result) { warning("ksip_alert: start playout failed (%d) for %s\n", result, device); },
                              [](NoSource *) {}, [](NoSource *) {}});

void destroy(void *arg) {
    auto *alert = static_cast<Alert *>(arg);
    g_core.playout_gone(alert);
    alert->inner = static_cast<struct auplay_st *>(mem_deref(alert->inner));
}
int allocate(struct auplay_st **out, const struct auplay *, struct auplay_prm *prm, const char *device, auplay_write_h *handler, void *arg) {
    if (!out || !prm || !handler) return EINVAL;
    auto *alert = static_cast<Alert *>(mem_zalloc(sizeof(Alert), destroy));
    if (!alert) return ENOMEM;
    alert->handler = handler;
    alert->arg = arg;
    alert->prm = *prm;
    alert->set_device(device);
    const int err = g_core.take_playout(alert);
    if (err) {
        // Never the core's player (its start did not happen): freed here
        // without the destructor telling the core anything.
        mem_destructor(alert, nullptr);
        alert->inner = static_cast<struct auplay_st *>(mem_deref(alert->inner));
        mem_deref(alert);
        return err;
    }
    *out = reinterpret_cast<struct auplay_st *>(alert);
    return 0;
}
} // namespace

int start() {
    g_clock.fire = [](Timer which) {
        if (!g_player) return;
        if (which == Timer::Linger) g_core.stop_idle();
        else if (which == Timer::HandBack) g_core.hand_back();
        else g_core.watch_tick();
    };
    return auplay_register(&g_player, baresip_auplayl(), "ksip_alert", allocate);
}
void stop() {
    g_clock.cancel_all();
    g_player = static_cast<struct auplay *>(mem_deref(g_player));
}
Outcome switch_speaker(const char *speaker) { return g_core.switch_devices(nullptr, speaker).speaker; }
void add_state(odict *audio) { playback_session::add_playout_state(audio, "alert", g_core); }
} // namespace alert_player
