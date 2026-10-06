// The alert sounds' player; see alert_player.h.
#include "audio_session.h"
#include "alert_player.h"
#include "alert_adm.h"
#include "alert_render.h"
#include "libre_clock.h"
#include "session_core.h"
#include "ksip_audio_bridge.h"
#include <cerrno>
#include <cstring>
#include <string>
#include <utility>
#include <baresip.h>

namespace alert_player {
namespace {
using playback_session::Core;
using playback_session::Outcome;
using playback_session::Timer;

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
    return (bridge != nullptr) && (ksip_audio_endpoint_listed(bridge, id, 1) != 0);
}
// The speaker the default stands for now, as the calls see it.
std::string default_speaker() {
    ksip_audio *bridge = playback_session::bridge();
    char id[KSIP_AUDIO_DEVICE_TEXT_SIZE] = {};
    if (bridge) ksip_audio_default_endpoint(bridge, 1, id, sizeof id);
    return id;
}
RenderAdm<WasapiRender> g_adm({usable,
                               [](const std::string &asked) {
                                   warning("ksip_alert: speaker %s is not there, the alert sound plays on the default communications speaker\n", asked.c_str());
                               },
                               default_speaker});
playback_session::LibreClock g_clock;
Core<Alert, NoSource> g_core(g_adm, g_clock,
                             {[](const char *line) {
                                  // The core's words are the call's; here they are the alert's.
                                  std::string said = line;
                                  if (said.rfind("ksip_audio:", 0) == 0) said.replace(0, std::strlen("ksip_audio"), "ksip_alert");
                                  for (const auto &[call, alert] : {std::pair{" WebRTC ADM", ""}, std::pair{"the call's", "the alert sound's"}, std::pair{"the call ", "the alert sound "}}) {
                                      for (size_t at = said.find(call); at != std::string::npos; at = said.find(call, at + std::strlen(alert)))
                                          said.replace(at, std::strlen(call), alert);
                                  }
                                  info("%s", said.c_str());
                              },
                              [](bool, const char *device, int result) { warning("ksip_alert: start playout failed (%d) for %s\n", result, device); },
                              [](NoSource *) {}, [](NoSource *) {}});

// The sound goes: the core lets its stream go (the render is closed, its
// thread joined) before the sound's memory does.
void destroy(void *arg) { g_core.playout_gone(static_cast<Alert *>(arg)); }
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
