// An attended transfer; see transfer.h.
#include "transfer.h"
#include "calls.h"
#include "ksip_text.h"
#include "sip_account.h"
#include <algorithm>
#include <vector>

namespace transfer {
namespace {
// The call the REFER goes to and the one it is to be replaced by: the call
// established first and the second, which the retry the other way round
// swaps.
std::string original, consultation, outcome;
// The call this module ends itself once a transfer has gone through. Its
// closing is part of the transfer, not a call the person hung up.
std::string transfer_hangup;
bool pending_ = false;
// The wait for the server to end the call it took over (transfer_leftover).
tmr transfer_timer;
// A transfer sends its REFER only once the holds it sent have been answered:
// the timer fires it, and these are the calls whose answer is still waited for.
tmr refer_timer;
std::vector<std::string> refer_waiting;
bool refer_armed = false;
bool transfer_reversed = false;
unsigned outcome_seq = 0;
// The REFER that is out: what identifies its transaction (read off the SIP
// trace as it goes), when it went, whether a 2xx has come for it, and
// whether a NOTIFY of its subscription has.
ksip_text::TransactionIds refer_sent;
uint64_t refer_sent_at = 0;
bool refer_accepted = false, refer_notified = false;
// The limit of a REFER, and the nudge a REFER that was accepted but tells
// nothing gets (see refer_deadline and refer_probe).
tmr deadline_timer, probe_timer;
// How soon a REFER has to be refused for the other way round to be tried:
// a server that does not take a transfer that way says so at once. A failure
// later (a subscription that ran out, a transfer that went wrong on the way)
// is not answered with a second REFER.
constexpr uint64_t REFUSED_WITHIN_MS = 5000;
// How long a 2xx to the REFER waits for its first NOTIFY before the calls
// are asked whether they are still there.
constexpr uint64_t NOTIFY_NUDGE_MS = 5000;

void clear() {
    original.clear();
    consultation.clear();
    pending_ = false;
    transfer_reversed = false;
    refer_armed = false;
    refer_waiting.clear();
    tmr_cancel(&refer_timer);
    refer_sent = {};
    refer_accepted = refer_notified = false;
    tmr_cancel(&deadline_timer);
    tmr_cancel(&probe_timer);
}
// The end of the wait for a REFER, the one limit a transfer keeps itself;
// the rest (a subscription's refresh, a re-INVITE's answer) is baresip's
// and libre's. It is 64*T1 from sending the REFER, SIP_T1 being libre's:
//  - With no answer at all, it is Timer F (RFC 3261 17.1.2.2): libre gives
//    the REFER up then, and baresip only logs that, so a transfer has to
//    see it by the time. The REFER most likely never reached the server;
//    the call it went to is resumed.
//  - With a 2xx but no NOTIFY, it is Timer N of RFC 6665 (4.1.2.4): a
//    subscriber with no NOTIFY within 64*T1 of sending its SUBSCRIBE takes
//    the subscription as failed. RFC 6665 says so of SUBSCRIBE; that it
//    holds for the subscription a REFER makes is our reading, since RFC 7647
//    has REFER rest on the RFC 6665 framework without restating the timer,
//    and RFC 3515 has the notifier send a NOTIFY at once but sets no limit.
//    The server took the REFER on and may be carrying the transfer out, so
//    the call the REFER went to is left on hold and the other one is resumed.
// A little is added so that libre, whose transaction runs the same 64*T1,
// has given up first. The outcome says only that it is not known. Referred
// the usual way, that resumes the first call with no answer and the second
// with no NOTIFY.
constexpr uint64_t REFER_LIMIT_MS = 64 * SIP_T1 + 2000;
void refer_deadline(void *) {
    std::string resume = refer_accepted ? consultation : original;
    debug("ksip: transfer: no %s within 64*T1 of the REFER, resuming %s\n", refer_accepted ? "NOTIFY" : "answer",
          refer_accepted ? "the call it was to be replaced by" : "the call it went to");
    clear();
    set_outcome("TRANSFER_UNKNOWN");
    if (auto c = calls::find(resume)) uag_hold_resume(c);
}
// A REFER answered 2xx whose NOTIFY has not come: each call gets a re-INVITE
// that changes nothing (the same offer, on hold as it is; RFC 3261 allows a
// re-INVITE that leaves the session as it was, and session timers, RFC 4028,
// refresh with one). A call the server no longer has is answered 481 or 408,
// on which libre ends it (RFC 5057), and it closes like any call; one that
// is still there answers and stays. Once, and with no wait of its own: the
// REFER's limit stays what ends the transfer.
void refer_probe(void *) {
    if (refer_notified) return;
    for (const std::string &id : {original, consultation}) {
        call *c = calls::find(id);
        if (!c || call_state(c) != CALL_STATE_ESTABLISHED) continue;
        debug("ksip: transfer: no NOTIFY %u ms after the 2xx, asking %s with a re-INVITE\n",
              static_cast<unsigned>(NOTIFY_NUDGE_MS), call_peeruri(c));
        call_modify(c);
    }
}
// A REFER is about to be handed to baresip: its limit counts from here. It is
// called before, since the REFER may pass the SIP trace (on_sip) before
// call_replace_transfer returns; one that could not be sent is cleared by
// the caller.
void refer_went() {
    refer_sent = {};
    refer_accepted = refer_notified = false;
    refer_sent_at = tmr_jiffies();
    tmr_cancel(&probe_timer);
    tmr_start(&deadline_timer, REFER_LIMIT_MS, refer_deadline, nullptr);
}
// Whether `a` was established before `b`: the call the consultation began
// from, whichever line it is on and whichever of the two was last taken off
// hold (a hold or a resume does not move the start). baresip's own attended
// transfer (menu atransferexec) refers that call, with the consultation call
// as its replacement. The production PBX carried out a transfer the other
// way round as well, but then sent neither NOTIFY nor BYE, and both calls
// stayed on screen. Two started in the same second go by the order the user
// agent made them in.
bool established_first(call *a, call *b) {
    uint32_t da = call_duration(a), db = call_duration(b);
    if (da != db) return da > db;
    for (le *l = list_head(ua_calls(call_get_ua(a))); l; l = l->next) {
        if (l->data == a) return true;
        if (l->data == b) return false;
    }
    return true;
}
// The REFER the other way round (the second call referred to the first),
// tried once when the first was refused. True when it went out.
bool refer_reversed(call *from, call *to) {
    transfer_reversed = true;
    std::swap(original, consultation);
    refer_went();
    if (!call_replace_transfer(to, from)) {
        debug("ksip: transfer: trying the other way round\n");
        return true;
    }
    std::swap(original, consultation);
    return false;
}
// The REFER of a transfer, once every hold sent for it has been answered (or
// the wait for them has run out): the first call is referred to the second,
// and a refusal is answered by trying the other way round once, as before.
void send_refer(void *) {
    if (!refer_armed) return;
    refer_armed = false;
    refer_waiting.clear();
    auto from = calls::find(original);
    auto to = calls::find(consultation);
    if (!from || !to || call_state(from) != CALL_STATE_ESTABLISHED || call_state(to) != CALL_STATE_ESTABLISHED) {
        clear();
        set_outcome("TRANSFER_FAILED");
        if (from) uag_hold_resume(from);
        return;
    }
    refer_went();
    if (!call_replace_transfer(from, to)) return;
    if (!refer_reversed(from, to)) {
        clear();
        set_outcome("TRANSFER_FAILED");
        uag_hold_resume(from);
    }
}
// The server takes the other call over with the transfer and ends it itself.
// A BYE from here could reach it before it has done so and undo the transfer
// (a PBX that reports the transfer done the instant it accepts the REFER
// behaves that way), so the call is left to the server for a while.
void transfer_leftover(void *) {
    auto c = calls::find(transfer_hangup);
    if (!c) {
        transfer_hangup.clear();
        return;
    }
    info("ksip: the server left the transferred call up, ending it\n");
    ua_hangup(call_get_ua(c), c, 0, nullptr);
}
} // namespace

bool idle() { return original.empty(); }
bool pending() { return pending_; }
void set_outcome(const char *code) {
    outcome = code;
    ++outcome_seq;
}
int start(const std::string &one, const std::string &other) {
    auto from = calls::find(one);
    auto to = calls::find(other);
    if (!from || !to || from == to || call_state(from) != CALL_STATE_ESTABLISHED || call_state(to) != CALL_STATE_ESTABLISHED) {
        clear();
        return EINVAL;
    }
    if (!established_first(from, to)) std::swap(from, to);
    original = call_id(from);
    consultation = call_id(to);
    transfer_reversed = false;
    // As RFC 5589 has it: both calls on hold, then the first call is
    // referred to the second. The REFER waits until every hold sent here has been answered
    // and acknowledged. The production PBX ignored a REFER that arrived
    // while a hold re-INVITE on either call was still in progress, and
    // that is why its transfers did not go through; a hold that gets no
    // answer is waited for two seconds at most.
    refer_waiting.clear();
    for (call *each : {from, to})
        if (!call_is_onhold(each)) {
            int err = call_hold(each, true);
            if (err) {
                clear();
                return err;
            }
            refer_waiting.push_back(call_id(each));
        }
    pending_ = true;
    refer_armed = true;
    set_outcome("TRANSFER_PENDING");
    tmr_start(&refer_timer, refer_waiting.empty() ? 0 : 2000, send_refer, nullptr);
    return 0;
}
void on_event(bevent_ev ev, bevent *e, call *c, const std::string &id) {
    if (ev == BEVENT_CALL_REMOTE_SDP && refer_armed && !str_cmp(bevent_get_text(e), "answer")) {
        // The answer to a hold sent for the transfer. Its ACK goes out right
        // after this event, so the REFER follows from the timer, not from here.
        refer_waiting.erase(std::remove(refer_waiting.begin(), refer_waiting.end(), id), refer_waiting.end());
        if (refer_waiting.empty()) tmr_start(&refer_timer, 0, send_refer, nullptr);
    }
    if (ev == BEVENT_CALL_TRANSFER_FAILED && id == original) {
        auto other = calls::find(consultation);
        // Both calls are on hold by now (the REFER waited for that), so the
        // other way round can go out at once; only for a refusal that came
        // straight away, though, which is how a server says it does not take
        // a transfer that way.
        uint64_t after = tmr_jiffies() - refer_sent_at;
        bool refused = after <= REFUSED_WITHIN_MS;
        if (!refused) debug("ksip: transfer: failed %llu ms after the REFER, not tried the other way round\n", static_cast<unsigned long long>(after));
        if (refused && !transfer_reversed && other && call_state(c) == CALL_STATE_ESTABLISHED &&
            call_state(other) == CALL_STATE_ESTABLISHED && refer_reversed(c, other))
            return;
        clear();
        set_outcome("TRANSFER_FAILED");
        uag_hold_resume(c);
    }
}
bool on_call_closed(call *, const std::string &id, const char *text) {
    if (id == original || id == consultation) {
        std::string other = id == original ? consultation : original;
        bool completed = id == original && !str_cmp(text, "Call transfered");
        bool was_original = id == original;
        clear();
        set_outcome(completed ? "TRANSFER_DONE" : was_original ? "TRANSFER_ORIGINAL_CLOSED" : "TRANSFER_OTHER_CLOSED");
        if (auto remaining = calls::find(other)) {
            if (completed) {
                transfer_hangup = other;
                info("ksip: transfer accepted, leaving the call with %s to the server\n", call_peeruri(remaining));
                tmr_start(&transfer_timer, 5000, transfer_leftover, nullptr);
            } else uag_hold_resume(remaining);
        }
        return true;
    }
    if (sip_account::user_agent() && !str_cmp(text, "Call transfered")) {
        // The transferred leg says so itself, whichever leg closed first. The
        // other one sometimes reports a reset before this arrives, and that
        // must not stand as the result.
        clear();
        set_outcome("TRANSFER_DONE");
        return true;
    }
    return false;
}
bool release_leftover(const std::string &id) {
    if (id != transfer_hangup) return false;
    transfer_hangup.clear();
    tmr_cancel(&transfer_timer);
    return true;
}
void write_state(odict *xfer) {
    odict_entry_add(xfer, "original", ODICT_STRING, original.c_str());
    odict_entry_add(xfer, "consultation", ODICT_STRING, consultation.c_str());
    odict_entry_add(xfer, "pending", ODICT_BOOL, pending_);
    odict_entry_add(xfer, "outcome", ODICT_STRING, outcome.c_str());
    odict_entry_add(xfer, "outcome_seq", ODICT_INT, static_cast<int64_t>(outcome_seq));
}
void on_sip(bool tx, const uint8_t *packet, size_t length) {
    if (!pending_ || !packet) return;
    if (tx) {
        // The REFER going out (a retransmission carries the same ids, a
        // resend after a challenge new ones): its 2xx is known by them.
        auto ids = ksip_text::request_ids(packet, length, "REFER");
        if (!ids.call_id.empty()) refer_sent = ids;
        return;
    }
    if (!refer_accepted && ksip_text::is_success_answer(refer_sent, packet, length)) {
        refer_accepted = true;
        debug("ksip: transfer: the REFER was accepted\n");
        if (!refer_notified) tmr_start(&probe_timer, NOTIFY_NUDGE_MS, refer_probe, nullptr);
        return;
    }
    // A NOTIFY of the REFER's subscription (in the same dialog, Event refer):
    // the outcome is baresip's to read from here on, however long it takes.
    auto notify = ksip_text::request_ids(packet, length, "NOTIFY");
    if (refer_notified || notify.call_id.empty() || notify.call_id != refer_sent.call_id) return;
    std::string event = ksip_text::sip_header(packet, length, "Event");
    if (event.empty()) event = ksip_text::sip_header(packet, length, "o");
    // The event name as libre matches it to the subscription (listen.c,
    // pl_strcmp): a NOTIFY libre would not hand to baresip tells nothing.
    if (event.compare(0, 5, "refer") != 0 || (event.size() > 5 && event[5] != ';' && event[5] != ' ')) return;
    refer_notified = true;
    tmr_cancel(&probe_timer);
    tmr_cancel(&deadline_timer);
    debug("ksip: transfer: the REFER's NOTIFY came\n");
}
void init() {
    tmr_init(&transfer_timer);
    tmr_init(&refer_timer);
    tmr_init(&deadline_timer);
    tmr_init(&probe_timer);
}
void close() { clear(); }
} // namespace transfer
