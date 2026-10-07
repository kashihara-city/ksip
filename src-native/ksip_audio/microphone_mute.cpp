// The microphone's mute carried into the calls; see microphone_mute.h.
#include "microphone_mute.h"
#include <re.h>
#include <baresip.h>
#include <cstdint>

namespace microphone_mute {
namespace {
// How often the word is put on the calls: a call that starts while the
// microphone is muted sends silence within this.
constexpr uint64_t kEveryMs = 250;
tmr g_timer;
bool g_muted = false;

// Every call's audio follows the word: muted while it says so, back when it
// does not. Only the app's own calls are muted this way (nothing else in
// KSIP mutes baresip's audio), so following it undoes nothing.
void apply(bool muted) {
    for (const le *u = list_head(uag_list()); u; u = u->next)
        for (const le *l = list_head(ua_calls(static_cast<ua *>(u->data))); l; l = l->next) {
            audio *a = call_audio(static_cast<call *>(l->data));
            if (a && audio_ismuted(a) != muted) audio_mute(a, muted);
        }
}
void tick(void *) {
    tmr_start(&g_timer, kEveryMs, tick, nullptr);
    apply(g_muted);
}
} // namespace

void start() {
    g_muted = false;
    tmr_init(&g_timer);
    tmr_start(&g_timer, kEveryMs, tick, nullptr);
}
void stop() { tmr_cancel(&g_timer); }
void set(bool muted) {
    if (muted != g_muted)
        info("ksip_audio: the microphone is %s on the device; calls send %s\n", muted ? "muted" : "unmuted", muted ? "silence" : "what it picks up");
    g_muted = muted;
    apply(muted);
}
} // namespace microphone_mute
