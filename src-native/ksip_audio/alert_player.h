// The alert sounds (the ringtone above all) on the chosen speaker, or on the
// Windows default communications speaker while the chosen one is not there:
// the rule the calls follow too (device_selection.cc). baresip's wasapi
// module plays them, as before; this player, "ksip_alert", only decides the
// endpoint wasapi is given, as each sound starts, so a speaker that is back
// is used again from the next sound on. Given an endpoint Windows still
// knows but cannot use, wasapi opened nothing and the phone rang silently.
//
// Threads: baresip's main thread (menu starts the sounds there). Lifetime:
// start() at module load, stop() at unload (device_module.cpp).
#pragma once

namespace alert_player {
int start();
void stop();
} // namespace alert_player
