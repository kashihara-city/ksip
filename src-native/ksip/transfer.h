// An attended transfer: the two calls it joins, the REFER that waits for
// the holds to be answered, the retry the other way round, the leftover
// call the server is left to end, and the outcome the window shows. It owns
// its timers and clears itself when the module closes.
//
// Threads: baresip's main thread only (commands, call events, its timers).
// Lifetime: module-static state for one transfer at a time; init() after the
// account, close() before it (ksip.cpp), and no re-initialisation.
#pragma once
#include <cmath>
#include <algorithm>
#include <re.h>
#include <baresip.h>
#include <string>

namespace transfer {
// No transfer is under way.
bool idle();
// A REFER has gone out and its answer is awaited: only hanging up is
// allowed meanwhile.
bool pending();
// The ksip_action transfer of the two calls, given in any order: once both
// are on hold, the one established first is referred to the other, as
// baresip's own attended transfer does, whichever line each is on.
int start(const std::string &one, const std::string &other);
// The outcome is counted as well as named: the window shows a notice each
// time one is set, and the same outcome twice in a row (returning to the
// held call after each of two second calls) would otherwise look unchanged.
void set_outcome(const char *code);
// The events a transfer follows.
void on_event(bevent_ev ev, bevent *e, call *c, const std::string &id);
// A closed call that was one of the transfer's legs, or the transferred leg
// saying so itself: true when the closing was the transfer's to handle.
bool on_call_closed(call *c, const std::string &id, const char *text);
// Every SIP message, as the trace passes it: the REFER going out, its 2xx
// and the NOTIFY of its subscription, which baresip does not report.
void on_sip(bool tx, const uint8_t *packet, size_t length);
// A closed call that was the one left to the server after a transfer:
// true when it was, and nothing more is to be done for it.
bool release_leftover(const std::string &id);
void write_state(odict *xfer);
void init();
void close();
} // namespace transfer
