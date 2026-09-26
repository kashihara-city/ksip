// The subscriptions; see subscriptions.h.
#include "subscriptions.h"
#include "sip_account.h"
#include "ksip_io.h"
#include <array>
#include <string>
#include <unordered_map>

namespace subscriptions {
namespace {
struct ParkSlot {
    struct sipsub *sub = nullptr;
    std::string number;
    std::string state = "UNKNOWN";
    // The dialogs the server has reported for this number, by id, so that a
    // partial report (RFC 4235) changes only the dialogs it names.
    std::unordered_map<std::string, std::string> dialogs;
};
// The numbers the buttons watch: up to thirty dialog subscriptions, six
// for the phone and the rest for the panel beside it.
std::array<ParkSlot, 30> parking;
// Retries the subscriptions a while after one closes.
tmr parking_timer;
// The voicemail box's message-summary subscription, and the last summary
// body the server sent ("Messages-Waiting: yes", "Voice-Message: 2/5").
// The app reads the counts out of it.
struct sipsub *mwi_sub = nullptr;
std::string mwi_summary;

int subscribe_parking();
int subscribe_mwi();

int auth_handler(char **username, char **password, const char *realm, void *arg) {
    return account_auth(static_cast<struct account *>(arg), username, password, realm);
}
void parking_notify(struct sip *sip, const struct sip_msg *msg, void *arg) {
    auto slot = static_cast<ParkSlot *>(arg);
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
void parking_retry(void *) {
    int err = subscribe_parking();
    if (subscribe_mwi()) err = EAGAIN;
    if (err) tmr_start(&parking_timer, 30000, parking_retry, nullptr);
}
// A subscription the server ends or refuses is written down with its answer:
// it explains buttons that stay unknown, and a quit that waits on the server.
void subscription_closed(const char *what, int err, const struct sip_msg *msg) {
    if (msg) info("ksip: %s subscription closed by %u %r\n", what, static_cast<unsigned>(msg->scode), &msg->reason);
    else info("ksip: %s subscription closed (%m)\n", what, err);
}
void parking_closed(int err, const struct sip_msg *msg, const struct sipevent_substate *, void *arg) {
    // The subscription is over, so its reference goes here, as baresip's own
    // presence module does. The server is asked again after a while rather
    // than only at the next registration, or the buttons would stay unknown
    // for minutes.
    auto slot = static_cast<ParkSlot *>(arg);
    subscription_closed(("parking " + slot->number).c_str(), err, msg);
    slot->sub = static_cast<sipsub *>(mem_deref(slot->sub));
    slot->state = "UNKNOWN";
    tmr_start(&parking_timer, 30000, parking_retry, nullptr);
}
void mwi_notify(struct sip *sip, const struct sip_msg *msg, void *) {
    mwi_summary = std::string(reinterpret_cast<const char *>(mbuf_buf(msg->mb)), mbuf_get_left(msg->mb));
    // The first notify is the proof that the server took the subscription.
    info("ksip: mwi notify, %zu bytes\n", mwi_summary.size());
    (void)sip_treply(nullptr, sip, msg, 200, "OK");
}
void mwi_closed(int err, const struct sip_msg *msg, const struct sipevent_substate *, void *) {
    subscription_closed("mwi", err, msg);
    mwi_sub = static_cast<sipsub *>(mem_deref(mwi_sub));
    mwi_summary.clear();
    tmr_start(&parking_timer, 30000, parking_retry, nullptr);
}
int subscribe_mwi() {
    auto account_ua = sip_account::user_agent();
    if (!sip_account::registered() || mwi_sub || sip_account::own_user().empty()) return 0;
    const char *routev[1] = {ua_outbound(account_ua)};
    std::string uri = "sip:" + sip_account::own_user() + "@" + sip_account::authority() + ";transport=" + sip_account::scheme();
    int err = sipevent_subscribe(&mwi_sub, uag_sipevent_sock(), uri.c_str(), nullptr, account_aor(ua_account(account_ua)), "message-summary",
                                 nullptr, 600, ua_cuser(account_ua), routev, routev[0] ? 1 : 0, auth_handler, ua_account(account_ua), true,
                                 nullptr, mwi_notify, mwi_closed, nullptr, "Accept: application/simple-message-summary\r\n");
    if (err) warning("ksip: mwi subscription to %s failed (%d)\n", uri.c_str(), err);
    else info("ksip: mwi subscription to %s\n", uri.c_str());
    return err;
}
void clear() {
    tmr_cancel(&parking_timer);
    if (mwi_sub) {
        auto sub = mwi_sub;
        mwi_sub = nullptr;
        mem_deref(sub);
    }
    mwi_summary.clear();
    for (auto &slot : parking) {
        auto sub = slot.sub;
        slot.sub = nullptr;
        slot.state = "UNKNOWN";
        if (sub) mem_deref(sub);
    }
}
int subscribe_parking() {
    auto account_ua = sip_account::user_agent();
    if (!sip_account::registered()) return 0;
    const char *routev[1] = {ua_outbound(account_ua)};
    int result = 0;
    for (auto &slot : parking) {
        if (slot.number.empty() || slot.sub) continue;
        std::string uri = ksip_io::sip_uri(slot.number) ? slot.number : "sip:" + slot.number + "@" + sip_account::authority() + ";transport=" + sip_account::scheme();
        int err = sipevent_subscribe(&slot.sub, uag_sipevent_sock(), uri.c_str(), nullptr, account_aor(ua_account(account_ua)), "dialog", nullptr,
                                     600, ua_cuser(account_ua), routev, routev[0] ? 1 : 0, auth_handler, ua_account(account_ua), true, nullptr,
                                     parking_notify, parking_closed, &slot, "Accept: application/dialog-info+xml\r\n");
        if (err) {
            slot.state = "UNKNOWN";
            result = err;
        }
    }
    return result;
}
} // namespace

void subscribe_all() {
    (void)subscribe_parking();
    (void)subscribe_mwi();
}
int configure(re_printf *pf, void *arg) {
    auto a = static_cast<cmd_arg *>(arg);
    // Up to thirty comma-separated numbers; an empty one is a slot nobody watches.
    std::array<std::string, 30> values;
    if (!a || !ksip_io::parse_watch_list(a->prm, values)) return EINVAL;
    clear();
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
void init() { tmr_init(&parking_timer); }
void close() { clear(); }
} // namespace subscriptions
