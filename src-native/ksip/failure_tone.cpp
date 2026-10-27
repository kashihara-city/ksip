// The tone of a call that failed; see failure_tone.h.
#include "failure_tone.h"
#include <cstdint>
#include <baresip.h>

namespace failure_tone {
namespace {
constexpr uint16_t kNotFound = 404;
constexpr uint16_t kBusyHere = 486;
constexpr uint16_t kTerminated = 487;
constexpr uint16_t kDecline = 603;
bool g_enabled = false;
play *g_play = nullptr;

// The menu's choice of file (errorcode_fb_aufile), none for a cancelled call.
const char *tone_for(uint16_t status) {
    switch (status) {
    case kNotFound: return "notfound.wav";
    case kBusyHere:
    case kDecline: return "busy.wav";
    case kTerminated: return nullptr;
    default: return "error.wav";
    }
}
void stop() { g_play = static_cast<play *>(mem_deref(g_play)); }
} // namespace

void init() {
    g_enabled = false;
    conf_get_bool(conf_cur(), "ksip_failure_tones", &g_enabled);
}
void close() { stop(); }
void on_event(bevent_ev ev, call *c) {
    if (!g_enabled) return;
    if (ev == BEVENT_CALL_INCOMING || ev == BEVENT_CALL_OUTGOING) {
        stop();
        return;
    }
    if (ev != BEVENT_CALL_CLOSED || !c) return;
    const uint16_t status = call_scode(c);
    const char *file = status != 0 ? tone_for(status) : nullptr;
    // As the menu: only for the call that closes as the last one up.
    if (!file || uag_call_count() != 1) return;
    const config *cfg = conf_config();
    const char *speaker = cfg && cfg->audio.play_dev[0] != '\0' ? cfg->audio.play_dev : "default";
    stop();
    const int err = play_file(&g_play, baresip_player(), file, 1, "ksip_alert", speaker);
    if (err) warning("ksip: the %s tone could not be played (%m)\n", file, err);
}
} // namespace failure_tone
