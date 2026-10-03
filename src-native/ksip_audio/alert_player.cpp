// The alert sounds' player; see alert_player.h.
#include "audio_session.h"
#include "alert_player.h"

namespace alert_player {
namespace {
struct auplay *g_player = nullptr;
// What baresip holds for a sound: the wasapi player doing the work. baresip
// only passes the pointer around and dereferences it.
struct Alert {
    struct auplay_st *inner;
};
void destroy(void *arg) { mem_deref(static_cast<Alert *>(arg)->inner); }

// Whether the chosen speaker is there, by the test the calls go by (the
// bridge's list of the endpoints WebRTC can open), so that the ringtone and
// the call are never on different speakers for it.
bool usable(const char *id) {
    ksip_audio *bridge = playback_session::bridge();
    return bridge && ksip_audio_endpoint_listed(bridge, id, 1);
}

int allocate(struct auplay_st **out, const struct auplay *, struct auplay_prm *prm, const char *device,
             auplay_write_h *handler, void *arg) {
    if (!out || !prm) return EINVAL;
    const bool chosen = device && *device && str_casecmp(device, "default") != 0;
    const char *endpoint = chosen && usable(device) ? device : "default";
    if (chosen && endpoint != device)
        warning("ksip_alert: speaker %s is not there, the alert sound plays on the default communications speaker\n", device);
    auto alert = static_cast<Alert *>(mem_zalloc(sizeof(Alert), destroy));
    if (!alert) return ENOMEM;
    const int err = auplay_alloc(&alert->inner, baresip_auplayl(), "wasapi", prm, endpoint, handler, arg);
    if (err) {
        mem_deref(alert);
        return err;
    }
    *out = reinterpret_cast<struct auplay_st *>(alert);
    return 0;
}
} // namespace

int start() { return auplay_register(&g_player, baresip_auplayl(), "ksip_alert", allocate); }
void stop() { g_player = static_cast<struct auplay *>(mem_deref(g_player)); }
} // namespace alert_player
