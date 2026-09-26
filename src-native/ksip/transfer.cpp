// An attended transfer; see transfer.h.
#include "transfer.h"
#include "calls.h"
#include "sip_account.h"
#include <algorithm>
#include <vector>

namespace transfer {
namespace {
std::string original, consultation, outcome;
// The call this module ends itself once a transfer has gone through. Its
// closing is part of the transfer, not a call the person hung up.
std::string transfer_hangup;
bool pending_ = false;
tmr transfer_timer;
// A transfer sends its REFER only once the holds it sent have been answered:
// the timer fires it, and these are the calls whose answer is still waited for.
tmr refer_timer;
std::vector<std::string> refer_waiting;
bool refer_armed = false;
bool transfer_reversed = false;
unsigned outcome_seq = 0;

void clear() {
    original.clear();
    consultation.clear();
    pending_ = false;
    transfer_reversed = false;
    tmr_cancel(&transfer_timer);
    refer_armed = false;
    refer_waiting.clear();
    tmr_cancel(&refer_timer);
}
// The REFER the other way round (call 2 referred to call 1), tried once when
// the first was refused. True when it went out.
bool refer_reversed(call *from, call *to) {
    transfer_reversed = true;
    std::swap(original, consultation);
    if (!call_replace_transfer(to, from)) return true;
    std::swap(original, consultation);
    return false;
}
// The REFER of a transfer, once every hold sent for it has been answered (or
// the wait for them has run out): call 1 is referred to call 2, and a refusal
// is answered by trying the other way round once, as before.
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
    if (call_replace_transfer(from, to) && !refer_reversed(from, to)) {
        clear();
        set_outcome("TRANSFER_FAILED");
        uag_hold_resume(from);
    }
}
void transfer_timeout(void *) {
    pending_ = false;
    refer_armed = false;
    set_outcome("TRANSFER_UNKNOWN");
    if (auto c = calls::find(original)) uag_hold_resume(c);
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
void begin_consult(const std::string &orig, const std::string &cons) {
    original = orig;
    consultation = cons;
    outcome.clear();
}
void set_outcome(const char *code) {
    outcome = code;
    ++outcome_seq;
}
int start(const std::string &orig, const std::string &cons) {
    if (original.empty()) {
        original = orig;
        consultation = cons;
    }
    auto from = calls::find(original);
    auto to = calls::find(consultation);
    if (!from || !to || from == to || call_state(from) != CALL_STATE_ESTABLISHED || call_state(to) != CALL_STATE_ESTABLISHED) {
        clear();
        return EINVAL;
    }
    transfer_reversed = false;
    // As RFC 5589 has it: both calls on hold, then call 1 is referred to
    // call 2. The REFER waits until every hold sent here has been answered
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
    tmr_start(&transfer_timer, 60000, transfer_timeout, nullptr);
    tmr_start(&refer_timer, refer_waiting.empty() ? 0 : 2000, send_refer, nullptr);
    return 0;
}
int cancel() {
    auto from = calls::find(original);
    auto to = calls::find(consultation);
    clear();
    set_outcome("TRANSFER_CANCELLED");
    if (to) ua_hangup(call_get_ua(to), to, 0, nullptr);
    return from ? uag_hold_resume(from) : 0;
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
        // other way round can go out at once.
        if (!transfer_reversed && other && call_state(c) == CALL_STATE_ESTABLISHED && call_state(other) == CALL_STATE_ESTABLISHED &&
            refer_reversed(c, other)) {
            tmr_start(&transfer_timer, 60000, transfer_timeout, nullptr);
            return;
        }
        pending_ = false;
        tmr_cancel(&transfer_timer);
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
void init() {
    tmr_init(&transfer_timer);
    tmr_init(&refer_timer);
}
void close() { clear(); }
} // namespace transfer
