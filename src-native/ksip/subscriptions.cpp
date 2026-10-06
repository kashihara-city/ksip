// The subscriptions; see subscriptions.h.
#include "subscriptions.h"
#include "sip_account.h"
#include "ksip_io.h"
#include <array>
#include <string>
#include <unordered_map>

namespace subscriptions {
namespace {
// When a subscription that ended is asked for again. RFC 6665 sets the rules
// and leaves the timing open:
//  - A refresh answered 404, 405, 410, 416, 480-485, 489, 501 or 604 MUST be
//    taken as the end of the subscription (4.1.2.2); asking again means a new
//    SUBSCRIBE, with a new Call-ID and From tag, which libre makes for each one.
//  - A NOTIFY that ends it gives the reason (4.1.3): deactivated SHOULD be
//    asked again at once, timeout MAY be; probation and giveup later, and not
//    before a retry-after; rejected, noresource and invariant SHOULD NOT be
//    asked again. With no reason or one not known, it MAY be asked again at
//    any time, again not before a retry-after.
//  - RFC 3261 (21.4.4) has a request answered 403 not repeated.
// How often and how many times is not set anywhere. So:
//  - Quick: a 481 (the server no longer has it; the production PBX answered
//    refreshes so now and then, and showed the button unknown for the 30
//    seconds the old single retry waited), or deactivated or timeout with no
//    retry-after. Asked again one second after each try's outcome, five times
//    at most, the first one spread over the first second so that every button
//    ending at once (a server restart) does not go back in one burst. The
//    budget comes back only once a NOTIFY proves the subscription works
//    again, so the slower retries never set off another round of quick ones.
//    Meanwhile the last known state stays shown.
//  - Later: anything else that is no refusal (no answer, 5xx, 480, 482-485, an
//    unknown reason), and a quick one that has used its budget. Asked again
//    every 30 seconds, or after the retry-after given if that is longer, for as
//    long as it takes: such a subscription is not given up. The state shows as
//    unknown.
//  - Stop: a refusal (403, 404, 405, 410, 416, 489, 501, 603, 604; rejected,
//    noresource, invariant). Not asked again until the numbers are set again
//    or the engine starts again. The state shows as unknown.
// Each subscription has its own timer, so that one ending does not move when
// another is asked again, and the re-registration (which subscribes what has
// no subscription) leaves the ones waiting or stopped alone.
enum class Retry { Quick, Later, Stop };
constexpr unsigned kQuickTries = 5;
constexpr uint64_t kQuickMs = 1000, kLaterMs = 30000;
struct RetryState {
    tmr timer;
    unsigned quick_left = kQuickTries;
    bool waiting = false, stopped = false;
};
struct ParkSlot {
    struct sipsub *sub = nullptr;
    std::string number;
    std::string state = "UNKNOWN";
    // The dialogs the server has reported for this number, by id, so that a
    // partial report (RFC 4235) changes only the dialogs it names.
    std::unordered_map<std::string, std::string> dialogs;
    RetryState retry;
};
// The numbers the buttons watch: up to WATCH_COUNT dialog subscriptions, six
// for the phone and the rest for the panel beside it.
std::array<ParkSlot, ksip_text::WATCH_COUNT> parking;
// The voicemail box's message-summary subscription, and the last summary
// body the server sent ("Messages-Waiting: yes", "Voice-Message: 2/5").
// The app reads the counts out of it.
struct sipsub *mwi_sub = nullptr;
std::string mwi_summary;
RetryState mwi_retry;

int subscribe_slot(ParkSlot &slot);
int subscribe_mwi();

int auth_handler(char **username, char **password, const char *realm, void *arg) {
    return account_auth(static_cast<struct account *>(arg), username, password, realm);
}
// How a subscription that ended is asked for again (see Retry), from how it
// ended: the NOTIFY that ended it, the answer that did, or an error with
// neither (no answer). `at_least` is the retry-after the server gave, in ms.
Retry how_to_retry(const struct sip_msg *msg, const struct sipevent_substate *substate, uint64_t &at_least) {
    at_least = 0;
    if (substate) {
        if (pl_isset(&substate->retry_after)) at_least = pl_u32(&substate->retry_after) * 1000ull;
        // Read as sent: libre does not know invariant.
        pl reason = PL_INIT;
        (void)msg_param_decode(&substate->params, "reason", &reason);
        if (!pl_strcasecmp(&reason, "rejected") || !pl_strcasecmp(&reason, "noresource") || !pl_strcasecmp(&reason, "invariant"))
            return Retry::Stop;
        if ((!pl_strcasecmp(&reason, "deactivated") || !pl_strcasecmp(&reason, "timeout")) && !at_least) return Retry::Quick;
        return Retry::Later;
    }
    if (msg && msg->scode >= 300) {
        if (auto retry_after = sip_msg_hdr(msg, SIP_HDR_RETRY_AFTER)) at_least = pl_u32(&retry_after->val) * 1000ull;
        switch (msg->scode) {
        case 403: case 404: case 405: case 410: case 416: case 489: case 501: case 603: case 604:
            return Retry::Stop;
        case 481:
            return at_least ? Retry::Later : Retry::Quick;
        default:
            return Retry::Later;
        }
    }
    return Retry::Later;
}
// Sets the next try of a subscription that ended `how`. True when it is a
// quick one, during which the state it last reported stays shown.
bool schedule(RetryState &retry, Retry how, uint64_t at_least, tmr_h *handler, void *arg, const std::string &what) {
    tmr_cancel(&retry.timer);
    retry.waiting = false;
    if (how == Retry::Stop) {
        retry.stopped = true;
        info("ksip: %s subscription refused, not asked for again until the numbers are set again\n", what.c_str());
        return false;
    }
    const bool quick = how == Retry::Quick && retry.quick_left;
    // The first quick try is spread over the first second, the others come a
    // second after the outcome of the one before.
    const uint64_t wait = !quick ? std::max(kLaterMs, at_least) : retry.quick_left == kQuickTries ? rand_u16() % kQuickMs : kQuickMs;
    if (quick) --retry.quick_left;
    retry.waiting = true;
    tmr_start(&retry.timer, wait, handler, arg);
    debug("ksip: %s subscription asked for again in %llu ms%s\n", what.c_str(), static_cast<unsigned long long>(wait),
          quick ? " (a quick try)" : "");
    return quick;
}
void parking_notify(struct sip *sip, const struct sip_msg *msg, void *arg) {
    auto slot = static_cast<ParkSlot *>(arg);
    // A NOTIFY is the proof that the subscription works: quick tries are
    // there again for the next time it ends.
    slot->retry.quick_left = kQuickTries;
    std::string body(reinterpret_cast<const char *>(mbuf_buf(msg->mb)), mbuf_get_left(msg->mb));
    auto info = ksip_io::read_dialog_info(body);
    if (!info.readable) {
        // Not something this reads (an empty body, another format): the state
        // stays what the last readable report said, never "free" by default.
        if (!body.empty()) warning("ksip: parking %s: notification not read as dialog-info, state kept\n", slot->number.c_str());
    } else {
        // A full report replaces what is known; a partial one changes only the
        // dialogs it names. Every state but terminated counts as in use.
        if (!info.partial) slot->dialogs.clear();
        for (auto &[id, state] : info.dialogs) {
            if (state == "terminated") slot->dialogs.erase(id);
            else slot->dialogs[id] = state;
        }
        slot->state = slot->dialogs.empty() ? "IDLE" : "INUSE";
    }
    (void)sip_treply(nullptr, sip, msg, 200, "OK");
}
// A subscription the server ends or refuses is written down with its answer:
// it explains buttons that stay unknown, and a quit that waits on the server.
void subscription_closed(const char *what, int err, const struct sip_msg *msg, const struct sipevent_substate *substate) {
    if (substate) {
        // The reason as sent, which libre may not know (invariant).
        pl reason = PL("none");
        (void)msg_param_decode(&substate->params, "reason", &reason);
        info("ksip: %s subscription ended by the server (reason %r)\n", what, &reason);
    }
    else if (msg) info("ksip: %s subscription closed by %u %r\n", what, static_cast<unsigned>(msg->scode), &msg->reason);
    else info("ksip: %s subscription closed (%m)\n", what, err);
}
void slot_retry(void *arg) {
    auto slot = static_cast<ParkSlot *>(arg);
    slot->retry.waiting = false;
    if (subscribe_slot(*slot) && !schedule(slot->retry, Retry::Later, 0, slot_retry, slot, "parking " + slot->number)) {
        slot->state = "UNKNOWN";
        slot->dialogs.clear();
    }
}
void parking_closed(int err, const struct sip_msg *msg, const struct sipevent_substate *substate, void *arg) {
    // The subscription is over, so its reference goes here, as baresip's own
    // presence module does. It is asked for again as Retry says, not only at
    // the next registration, or the button would stay unknown for minutes.
    auto slot = static_cast<ParkSlot *>(arg);
    const std::string what = "parking " + slot->number;
    subscription_closed(what.c_str(), err, msg, substate);
    slot->sub = static_cast<sipsub *>(mem_deref(slot->sub));
    uint64_t at_least = 0;
    const Retry how = how_to_retry(msg, substate, at_least);
    if (!schedule(slot->retry, how, at_least, slot_retry, slot, what)) {
        slot->state = "UNKNOWN";
        slot->dialogs.clear();
    }
}
void mwi_notify(struct sip *sip, const struct sip_msg *msg, void *) {
    mwi_retry.quick_left = kQuickTries;
    mwi_summary = std::string(reinterpret_cast<const char *>(mbuf_buf(msg->mb)), mbuf_get_left(msg->mb));
    // The first notify is the proof that the server took the subscription.
    info("ksip: mwi notify, %zu bytes\n", mwi_summary.size());
    (void)sip_treply(nullptr, sip, msg, 200, "OK");
}
void mwi_retry_now(void *) {
    mwi_retry.waiting = false;
    if (subscribe_mwi() && !schedule(mwi_retry, Retry::Later, 0, mwi_retry_now, nullptr, "mwi")) mwi_summary.clear();
}
void mwi_closed(int err, const struct sip_msg *msg, const struct sipevent_substate *substate, void *) {
    subscription_closed("mwi", err, msg, substate);
    mwi_sub = static_cast<sipsub *>(mem_deref(mwi_sub));
    uint64_t at_least = 0;
    const Retry how = how_to_retry(msg, substate, at_least);
    if (!schedule(mwi_retry, how, at_least, mwi_retry_now, nullptr, "mwi")) mwi_summary.clear();
}
int subscribe_mwi() {
    auto account_ua = sip_account::user_agent();
    if (!sip_account::registered() || mwi_sub || mwi_retry.waiting || mwi_retry.stopped || sip_account::own_user().empty()) return 0;
    const char *routev[1] = {ua_outbound(account_ua)};
    std::string uri = "sip:" + sip_account::own_user() + "@" + sip_account::authority() + ";transport=" + sip_account::scheme();
    int err = sipevent_subscribe(&mwi_sub, uag_sipevent_sock(), uri.c_str(), nullptr, account_aor(ua_account(account_ua)), "message-summary",
                                 nullptr, 600, ua_cuser(account_ua), routev, routev[0] ? 1 : 0, auth_handler, ua_account(account_ua), true,
                                 nullptr, mwi_notify, mwi_closed, nullptr, "Accept: application/simple-message-summary\r\n");
    if (err) warning("ksip: mwi subscription to %s failed (%d)\n", uri.c_str(), err);
    else info("ksip: mwi subscription to %s\n", uri.c_str());
    return err;
}
// A retry state as it is before anything ended: the timer stopped, every
// quick try there, nothing waited for or refused.
void reset(RetryState &retry) {
    tmr_cancel(&retry.timer);
    retry.quick_left = kQuickTries;
    retry.waiting = retry.stopped = false;
}
// Ends the parking subscriptions and forgets what they reported: a slot
// that is subscribed again starts from a full report, and one whose number
// changed must not keep the dialogs of the old number. A number refused
// before is asked for again.
void clear_parking() {
    for (auto &slot : parking) {
        auto sub = slot.sub;
        slot.sub = nullptr;
        slot.state = "UNKNOWN";
        slot.dialogs.clear();
        reset(slot.retry);
        if (sub) mem_deref(sub);
    }
}
// The message summary subscription lives with the registration, not with
// the numbers the buttons watch; only the shutdown and the module's close
// end it.
void clear_mwi() {
    reset(mwi_retry);
    if (mwi_sub) {
        auto sub = mwi_sub;
        mwi_sub = nullptr;
        mem_deref(sub);
    }
    mwi_summary.clear();
}
void clear() {
    clear_mwi();
    clear_parking();
}
// Subscribes one watched number, unless it has a subscription, waits for
// its next try, or was refused.
int subscribe_slot(ParkSlot &slot) {
    auto account_ua = sip_account::user_agent();
    if (!sip_account::registered() || slot.number.empty() || slot.sub || slot.retry.waiting || slot.retry.stopped) return 0;
    const char *routev[1] = {ua_outbound(account_ua)};
    std::string uri = ksip_io::sip_uri(slot.number) ? slot.number : "sip:" + slot.number + "@" + sip_account::authority() + ";transport=" + sip_account::scheme();
    int err = sipevent_subscribe(&slot.sub, uag_sipevent_sock(), uri.c_str(), nullptr, account_aor(ua_account(account_ua)), "dialog", nullptr,
                                 600, ua_cuser(account_ua), routev, routev[0] ? 1 : 0, auth_handler, ua_account(account_ua), true, nullptr,
                                 parking_notify, parking_closed, &slot, "Accept: application/dialog-info+xml\r\n");
    if (err) slot.state = "UNKNOWN";
    return err;
}
// Subscribes every watched number that is to be subscribed now; one that
// fails at once (no route yet, a socket error) is tried again later. The
// first error, if any, is returned.
int subscribe_parking() {
    int result = 0;
    for (auto &slot : parking) {
        int err = subscribe_slot(slot);
        if (!err) continue;
        if (!result) result = err;
        schedule(slot.retry, Retry::Later, 0, slot_retry, &slot, "parking " + slot.number);
    }
    return result;
}
} // namespace

void subscribe_all() {
    (void)subscribe_parking();
    if (subscribe_mwi()) schedule(mwi_retry, Retry::Later, 0, mwi_retry_now, nullptr, "mwi");
}
// The registration came back after a failure (the PBX restarted, the network
// went): the server may have lost the subscriptions while it was away, and
// would say so only at their next refresh, up to nine minutes on, the buttons
// missing every change meanwhile. So each one that is up is ended and asked
// for anew as a quick try (see Retry): spread over the first second, against
// a server that has just come back to many phones at once, the state it last
// reported shown until the new NOTIFY, and the quick tries all there again,
// as for a subscription that starts. One that waits for its next try keeps
// it, and one refused stays so.
void resubscribe_all() {
    size_t anew = 0;
    for (auto &slot : parking) {
        if (!slot.sub || slot.retry.stopped) continue;
        slot.sub = static_cast<sipsub *>(mem_deref(slot.sub));
        slot.retry.quick_left = kQuickTries;
        schedule(slot.retry, Retry::Quick, 0, slot_retry, &slot, "parking " + slot.number);
        ++anew;
    }
    if (mwi_sub && !mwi_retry.stopped) {
        mwi_sub = static_cast<sipsub *>(mem_deref(mwi_sub));
        mwi_retry.quick_left = kQuickTries;
        schedule(mwi_retry, Retry::Quick, 0, mwi_retry_now, nullptr, "mwi");
        ++anew;
    }
    // None before the first registration: nothing to say then.
    if (anew) info("ksip: the registration is back after a failure, %zu subscriptions are asked for anew\n", anew);
}
int configure(re_printf *pf, void *arg) {
    auto a = static_cast<cmd_arg *>(arg);
    // Up to WATCH_COUNT comma-separated numbers; an empty one is a slot nobody watches.
    std::array<std::string, ksip_text::WATCH_COUNT> values;
    if (!a || !ksip_io::parse_watch_list(a->prm, values)) return EINVAL;
    // Only the parking subscriptions change hands here; the message summary
    // stays subscribed, whichever order the registration and this command
    // came in, and keeps whatever retry it has.
    clear_parking();
    for (size_t i = 0; i < values.size() && i < parking.size(); ++i) parking[i].number = values[i];
    int err = subscribe_parking();
    if (!err) re_hprintf(pf, "Parking subscriptions configured\n");
    return err;
}
int shutdown(re_printf *pf, void *) {
    // Each subscription ended here is a request the server still has to
    // answer before baresip can quit; the count says what a slow quit waits on.
    size_t watched = 0;
    for (auto &slot : parking) if (slot.sub) ++watched;
    info("ksip: shutdown, ending %zu parking subscriptions%s\n", watched, mwi_sub ? " and the mwi subscription" : "");
    clear();
    return re_hprintf(pf, "KSIP subscriptions closed\n");
}
void write_state(odict *od) {
    odict_entry_add(od, "mwi_summary", ODICT_STRING, mwi_summary.c_str());
    odict *parks = nullptr;
    if (odict_alloc(&parks, 8)) return;
    for (size_t i = 0; i < parking.size(); ++i) {
        odict *entry = nullptr;
        if (odict_alloc(&entry, 4)) continue;
        odict_entry_add(entry, "number", ODICT_STRING, parking[i].number.c_str());
        odict_entry_add(entry, "state", ODICT_STRING, parking[i].state.c_str());
        odict_entry_add(parks, std::to_string(i).c_str(), ODICT_OBJECT, entry);
        mem_deref(entry);
    }
    odict_entry_add(od, "parking", ODICT_ARRAY, parks);
    mem_deref(parks);
}
void init() {
    for (auto &slot : parking) tmr_init(&slot.retry.timer);
    tmr_init(&mwi_retry.timer);
}
void close() { clear(); }
} // namespace subscriptions
