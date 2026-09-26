// The subscriptions this phone holds at the server: the dialog state of the
// numbers the buttons watch (RFC 4235), and the message summary of its own
// voicemail box. It owns the slots, the retry timer and their release.
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
