// The alert sounds (the ringtone above all) on the chosen speaker, by the
// rule the calls follow (session_core.h): the chosen speaker while it is
// there, the Windows default communications speaker in its place while it
// is not, back on the chosen one when it is back, moved when the person
// chooses another during the sound, and opened again on the default when
// the speaker in use goes away. This player, "ksip_alert", has a session
// core of its own (a second instance of the calls') decide which endpoint
// a sound is opened on and when it is opened again, and a WASAPI stream of
// its own (alert_render.h) play it, opened on the endpoint the core decides
// on with the result and the endpoint's own id known at once. How things
// stand is in the module's state ("alert", audio_state.h), not in the log.
//
// Threads: baresip's main thread (menu starts the sounds there, the timers
// fire there); the stream's feed thread is the render's own. Lifetime:
// start() at module load, stop() at unload (device_module.cpp).
#pragma once
#include <re.h>
#include <baresip.h>
#include "session_core.h"

namespace alert_player {
int start();
void stop();
// The speaker chosen anew while a sound may be playing (device_module.cpp's
// switch): what came of it, as for the call's speaker.
playback_session::Outcome switch_speaker(const char *speaker);
// The "alert" part of the module's state (audio_state.h), added to `audio`.
void add_state(odict *audio);
} // namespace alert_player
