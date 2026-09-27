// The subscriptions this phone holds at the server: the dialog state of the
// numbers the buttons watch (RFC 4235), and the message summary of its own
// voicemail box. It owns the slots, the retry timer and their release.
//
// Threads: baresip's main thread only (commands, SIP events, the timer).
// Lifetime: module-static state; init() once at module load, close() at
// unload after the calls and the transfer (ksip.cpp), never re-initialised.
// Every subscription's reference is dropped from its own closed handler or
// from clear(); the retry timer is the only thing that outlives a
// registration, and it is cancelled in close().
#pragma once
#include <cmath>
#include <algorithm>
#include <re.h>
#include <baresip.h>

namespace subscriptions {
// Subscribes to everything not yet subscribed, once the account is registered.
void subscribe_all();
// The ksip_parking command: the numbers to watch, comma-separated.
int configure(re_printf *pf, void *arg);
// The ksip_shutdown command: every subscription is ended, so that baresip
// can quit without waiting on the server for them.
int shutdown(re_printf *pf, void *arg);
// The parking array and the message summary of the state reply.
void write_state(odict *od);
void init();
void close();
} // namespace subscriptions
