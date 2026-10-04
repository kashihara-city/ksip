// The alert sounds (the ringtone above all) on the chosen speaker, by the
// rule the calls follow (session_core.h): the chosen speaker while it is
// there, the Windows default communications speaker in its place while it
// is not, back on the chosen one when it is back, moved when the person
// chooses another during the sound, and opened again on the default when
// the speaker in use goes away. baresip's wasapi module plays the sound, as
// before; this player, "ksip_alert", wraps one wasapi player per sound and
// has a session core of its own decide which endpoint it is opened on and
// when it is opened again. How things stand is in the module's state
// ("alert", audio_state.h), not in the log.
//
// Threads: baresip's main thread (menu starts the sounds there, the timers
// fire there). Lifetime: start() at module load, stop() at unload
// (device_module.cpp).
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
