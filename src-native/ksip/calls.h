// The calls: what the window does to them (dial, answer, hold, transfer
// them blind, send digits), what happens to the others when one changes,
// the identity a PBX asserts for a caller, and the two switches that refuse
// incoming calls (do not disturb, maintenance).
//
// Threads: baresip's main thread only (commands and call events). Lifetime:
// module-static state (the identities, the two switches); close() at module
// unload, first of the ksip parts (ksip.cpp), and no re-initialisation.
#pragma once
#include <cmath>
#include <algorithm>
#include <re.h>
#include <baresip.h>
#include <string>
#include "ksip_io.h"

namespace calls {
call *find(const std::string &id);
// Puts every call being talked on, other than the given one, on hold.
int hold_others(const call *except);
// A P-Asserted-Identity arrived for a call: the number the PBX asserts, and
// the name if it gave one.
void note_identity(const std::string &callid, const std::string &uri, const std::string *name);
// A call closed: what was asserted for it goes.
void forget(const std::string &id);
// The call events that are about the calls themselves. True when the event
// was an incoming call refused here, which nothing else should handle.
bool on_event(bevent_ev ev, const bevent *e, call *c, const std::string &id);
// A call that ended and was not a transfer's: the first call is brought
// back when the second one ends.
void on_call_closed(const call *c, const std::string &id);
// The ksip_action command, once read.
int action(re_printf *pf, const ksip_io::ActionRequest &request);
// The calls array of the state reply, and the switches; returns how many
// calls it wrote.
unsigned write_state(odict *od, odict *list);
void close();
} // namespace calls
