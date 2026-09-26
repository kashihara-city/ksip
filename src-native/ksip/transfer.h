// An attended transfer: the two calls it joins, the REFER that waits for
// the holds to be answered, the retry the other way round, the leftover
// call the server is left to end, and the outcome the window shows. It owns
// its timers and clears itself when the module closes.
#pragma once
#include <cmath>
#include <algorithm>
#include <re.h>
#include <baresip.h>
#include <string>

namespace transfer {
// No transfer or consultation is under way.
bool idle();
// A REFER has gone out and its answer is awaited: only hanging up and
// calling the transfer off are allowed meanwhile.
bool pending();
// A consultation call was placed for `original`; its id is `consultation`.
void begin_consult(const std::string &original, const std::string &consultation);
// The ksip_action transfer: call `original` referred to call `consultation`
// once both are on hold.
int start(const std::string &original, const std::string &consultation);
// The ksip_action cancel_transfer: the consultation call is ended and the
// original resumed.
int cancel();
// The outcome is counted as well as named: the window shows a notice each
// time one is set, and the same outcome twice in a row (returning to the
// held call after each of two second calls) would otherwise look unchanged.
void set_outcome(const char *code);
// The events a transfer follows.
void on_event(bevent_ev ev, bevent *e, call *c, const std::string &id);
// A closed call that was one of the transfer's legs, or the transferred leg
// saying so itself: true when the closing was the transfer's to handle.
bool on_call_closed(call *c, const std::string &id, const char *text);
// A closed call that was the one left to the server after a transfer:
// true when it was, and nothing more is to be done for it.
bool release_leftover(const std::string &id);
void write_state(odict *xfer);
void init();
void close();
} // namespace transfer
