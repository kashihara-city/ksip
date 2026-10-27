// The tone of a call that failed (busy, not found, an error) on the call's
// speaker. baresip's menu plays it on the alert device, which is the
// ringtone's speaker; KSIP lets the ringtone go to a speaker of its own (a
// loud one across the room, say), while the tone answers the call the person
// made on the handset. So the engine's config tells the menu not to
// (busy_aufile none and the like, engine_config.rs) and turns this on
// (ksip_failure_tones yes), which plays the same files by the menu's rule:
// when the last call closes with a status, 404 the not found tone, 486 and
// 603 the busy one, 487 (cancelled) none, any other the error one. They go
// through ksip_alert, which takes the files as they are (8 kHz) and opens
// the default communications speaker in place of one that is not there, as
// the calls do, here on the call's speaker. The next call stops it.
//
// Threads: baresip's main thread (the events). Lifetime: init() at module
// load, close() at unload.
#pragma once
#include <re.h>
#include <baresip.h>

namespace failure_tone {
void init();
void close();
void on_event(bevent_ev ev, call *c);
} // namespace failure_tone
