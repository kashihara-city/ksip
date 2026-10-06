// The calls; see calls.h.
#include "calls.h"
#include "sip_account.h"
#include "transfer.h"
#include "inband_dtmf.h"
#include "ksip_io.h"
#include <cstring>
#include <unordered_map>
#include <cerrno>
#include <cstdint>
#include <string>
#include <baresip.h>

namespace calls {
using namespace ksip_io;
namespace {
std::unordered_map<std::string, std::string> connected_identity;
// The name a P-Asserted-Identity carried for a call, when it carried one.
std::unordered_map<std::string, std::string> connected_name;
// Do not disturb: an incoming call is answered with 486 Busy Here, so the PBX
// treats the phone as busy rather than absent. Never kept across a start.
bool dnd = false;
// Maintenance: the app is changing settings or calibrating and wants no
// call to arrive meanwhile, so an incoming call is refused as under do not
// disturb. Set and cleared by the app; never kept across a start.
bool maintenance = false;

// Whether some other call is being talked on right now.
bool talking_elsewhere(const call *except) {
    for (const le *u = list_head(uag_list()); u; u = u->next)
        for (const le *l = list_head(ua_calls(static_cast<ua *>(u->data))); l; l = l->next) {
            auto other = static_cast<call *>(l->data);
            if (other != except && call_state(other) == CALL_STATE_ESTABLISHED && !call_is_onhold(other)) return true;
        }
    return false;
}
} // namespace

call *find(const std::string &id) { return id.empty() ? nullptr : uag_call_find(id.c_str()); }
// The module does this itself (call_hold_other_calls is off): baresip would
// also do it when a second call is answered by the far end, and that took
// the person away from the call they had gone back to.
int hold_others(const call *except) {
    int err = 0;
    for (const le *u = list_head(uag_list()); u; u = u->next)
        for (const le *l = list_head(ua_calls(static_cast<ua *>(u->data))); l; l = l->next) {
            auto other = static_cast<call *>(l->data);
            if (other != except && call_state(other) == CALL_STATE_ESTABLISHED && !call_is_onhold(other)) err |= call_hold(other, true);
        }
    return err;
}
void note_identity(const std::string &callid, const std::string &uri, const std::string *name) {
    connected_identity[callid] = uri;
    if (name) connected_name[callid] = *name;
}
bool on_event(bevent_ev ev, const bevent *e, call *c, const std::string &) {
    if (ev == BEVENT_CALL_ESTABLISHED && talking_elsewhere(c)) {
        // The far end answered a call while the person was talking on another
        // one (they had gone back to it while this one rang). The person stays
        // where they are; this call waits on hold until they switch to it.
        info("ksip: call answered while another is active, holding it\n");
        call_hold(c, true);
    }
    if (ev == BEVENT_CALL_INCOMING && (dnd || maintenance)) {
        // The app hears of it through CALL_INCOMING and the CALL_CLOSED that
        // follows, with these words; the state reply carries no list of its own.
        info("ksip: %s, incoming call refused as busy\n", dnd ? "dnd" : "maintenance");
        ua_hangup(bevent_get_ua(e), c, ksip_io::sip_status::BUSY_HERE, "Busy Here");
        return true;
    }
    return false;
}
void on_call_closed(const call *c, const std::string &id) {
    auto account_ua = sip_account::user_agent();
    if (!account_ua) return;
    // Calling a second person holds the first call. When the second one
    // ends, whether it was answered or still ringing, the first is brought
    // back here: baresip does not do that by itself, and the window says
    // the call was returned to.
    int others = 0;
    call *remaining = nullptr;
    for (const le *l = list_head(ua_calls(account_ua)); l; l = l->next)
        if (static_cast<call *>(l->data) != c) {
            ++others;
            remaining = static_cast<call *>(l->data);
        }
    if (transfer::release_leftover(id)) {
    } else if (others == 1 && call_state(remaining) == CALL_STATE_ESTABLISHED) {
        transfer::set_outcome("TRANSFER_OTHER_CLOSED");
        if (call_is_onhold(remaining)) uag_hold_resume(remaining);
    } else if (others == 1) {
        // The remaining call is still ringing (either way); the window moves
        // to it, and says so with words that do not claim it was on hold.
        transfer::set_outcome("CALL_ENDED_SWITCHED");
    }
}
int action(re_printf *pf, const ActionRequest &request) {
    const std::string &op = request.op, &id = request.id, &value = request.value;
    call *c = find(id);
    int err = 0;
    // Hanging up is the way out of a pending transfer.
    if (transfer::pending() && op != "hangup") return EBUSY;
    if (op == "select") {
        // A held call comes back and the active one goes on hold. The active
        // call itself is left alone: uag_hold_resume on a call that is not on
        // hold resumes whichever other call is, which swapped the two calls
        // when the person clicked the line they were already talking on.
        if (c && call_state(c) == CALL_STATE_ESTABLISHED) return call_is_onhold(c) ? uag_hold_resume(c) : 0;
        return hold_others(c);
    }
    if (op == "dial") {
        auto account_ua = sip_account::user_agent();
        if (!sip_account::registered()) return EAGAIN;
        if (!transfer::idle() || !(sip_uri(value) ? address_ok(value) : token(value.c_str(), "*#+"))) return EINVAL;
        // A number is completed with the configured registrar; a URI is sent as
        // it was written. Either way the library has to be able to read it.
        const std::string uri = sip_account::uri_for(value);
        struct uri decoded{};
        struct pl span;
        pl_set_str(&span, uri.c_str());
        if (uri_decode(&decoded, &span)) return EINVAL;
        for (const le *l = list_head(ua_calls(account_ua)); l; l = l->next) {
            auto other = static_cast<call *>(l->data);
            if (call_state(other) == CALL_STATE_ESTABLISHED && !call_is_onhold(other)) {
                c = other;
                err = call_hold(other, true);
                if (err) return err;
            }
        }
        call *next = nullptr;
        err = ua_connect(account_ua, &next, nullptr, uri.c_str(), VIDMODE_OFF);
        if (err) {
            if (c) uag_hold_resume(c);
            return err;
        }
        return re_hprintf(pf, "%s", call_id(next));
    }
    if (op == "transfer") return transfer::start(id, value);
    if (op == "blind_transfer") {
        // The call in progress is sent to the number as it is; the buttons
        // decide what the number means (a park slot, a colleague, a queue).
        if (!c || call_state(c) != CALL_STATE_ESTABLISHED || call_is_onhold(c)) return EINVAL;
        // A URI goes into Refer-To as it was set up, angle brackets included:
        // baresip copies what it can read as it is, and the PBX may only take
        // the form its phones send. A number is completed with the registrar.
        std::string bare = value;
        if (bare.size() >= 2 && bare.front() == '<' && bare.back() == '>') bare = bare.substr(1, bare.size() - 2);
        if (!(sip_uri(bare) ? address_ok(value) : token(value.c_str(), "*#+"))) return EINVAL;
        const std::string uri = sip_uri(bare) ? value : "sip:" + escape_user(value) + "@" + sip_account::authority() + ";transport=" + sip_account::scheme();
        return call_transfer(c, uri.c_str());
    }
    // Do not disturb is about the account as well: on or off, no call named.
    if (op == "dnd") {
        dnd = value == "on";
        return 0;
    }
    // Maintenance is taken in one step: only while no call is up, so that a
    // call arriving between the app's look at the state and this is not
    // taken in and then worked around. Leaving it always succeeds.
    if (op == "maintenance") {
        if (value != "on") {
            maintenance = false;
            return 0;
        }
        for (const le *u = list_head(uag_list()); u; u = u->next)
            for (const le *l = list_head(ua_calls(static_cast<ua *>(u->data))); l; l = l->next)
                if (call_state(static_cast<call *>(l->data)) != CALL_STATE_TERMINATED) return EBUSY;
        maintenance = true;
        return 0;
    }
    // Unregistering is about the account, not about a call.
    if (op == "unregister") return sip_account::unregister();
    if (!c) return ENOENT;
    if (op == "answer") {
        err = hold_others(c);
        return err ? err : ua_answer(call_get_ua(c), c, VIDMODE_OFF);
    }
    if (op == "hangup") {
        ua_hangup(call_get_ua(c), c, 0, nullptr);
        return 0;
    }
    if (op == "hold") return call_state(c) == CALL_STATE_ESTABLISHED ? call_hold(c, true) : EINVAL;
    if (op == "resume") return call_state(c) == CALL_STATE_ESTABLISHED ? uag_hold_resume(c) : EINVAL;
    if (op == "dtmf" && value.size() == 1 && strchr("0123456789*#", value[0])) {
        if (!c || call_state(c) != CALL_STATE_ESTABLISHED) return EINVAL;
        // In band: the tones go out as sound on this call (inband_dtmf.h).
        if (request.mode == "inband") return inband_dtmf::queue(call_audio(c), value[0]) ? 0 : EOVERFLOW;
        if (request.mode != "info" && request.mode != "rtp") return EINVAL;
        // The account's mode is what baresip reads for each digit; it is set
        // for each digit, so that a changed setting needs no restart.
        account_set_dtmfmode(call_account(c), request.mode == "info" ? DTMFMODE_SIP_INFO : DTMFMODE_RTP_EVENT);
        // An RFC 4733 event needs the peer to have taken telephone-event;
        // without it baresip drops the digit without a word, so this says so.
        if (request.mode == "rtp" && !sdp_media_rformat(stream_sdpmedia(audio_strm(call_audio(c))), telev_rtpfmt)) {
            (void)re_hprintf(pf, "DTMF_RTP_NOT_OFFERED");
            return ENOTSUP;
        }
        err = call_send_digit(c, value[0]);
        if (!err) err = call_send_digit(c, KEYCODE_REL);
        return err;
    }
    return EINVAL;
}
// Room for a codec's name and rate, "opus 48000Hz" and the like.
constexpr size_t kCodecTextSize = 64;
unsigned write_state(odict *od, odict *list) {
    odict_entry_add(od, "dnd", ODICT_BOOL, dnd ? 1 : 0);
    unsigned index = 0;
    for (const le *u = list_head(uag_list()); u; u = u->next) {
        for (const le *l = list_head(ua_calls(static_cast<ua *>(u->data))); l; l = l->next) {
            auto c = static_cast<call *>(l->data);
            if (call_state(c) == CALL_STATE_TERMINATED) continue;
            odict *entry = nullptr;
            if (odict_alloc(&entry, ksip_io::DICT_BUCKETS)) continue;
            odict_entry_add(entry, "id", ODICT_STRING, call_id(c));
            const auto identity = connected_identity.find(call_id(c));
            odict_entry_add(entry, "peer", ODICT_STRING, identity == connected_identity.end() ? call_peeruri(c) : identity->second.c_str());
            // The caller's name: the one PAI gave, else the one From gave.
            const auto named = connected_name.find(call_id(c));
            pl from_name;
            pl_set_str(&from_name, call_peername(c) ? call_peername(c) : "");
            odict_entry_add(entry, "name", ODICT_STRING, (named == connected_name.end() ? display_name(from_name) : named->second).c_str());
            odict_entry_add(entry, "state", ODICT_STRING, call_statename(c));
            odict_entry_add(entry, "held", ODICT_BOOL, static_cast<int>(call_is_onhold(c)));
            odict_entry_add(entry, "duration", ODICT_INT, static_cast<int64_t>(call_duration(c)));
            auto audio = call_audio(c);
            if (auto codec = audio ? audio_codec(audio, true) : nullptr) {
                char described[kCodecTextSize];
                if (re_snprintf(described, sizeof(described), "%s %uHz", codec->name, codec->srate) > 0)
                    odict_entry_add(entry, "codec", ODICT_STRING, described);
            }
            // True only once the encryption is actually established, which for
            // DTLS is a moment after the call is answered.
            odict_entry_add(entry, "secure", ODICT_BOOL, static_cast<int>((audio != nullptr) && stream_is_secure(audio_strm(audio))));
            odict_entry_add(entry, "transport", ODICT_STRING, sip_transp_name(call_transp(c)));
            odict_entry_add(list, std::to_string(index++).c_str(), ODICT_OBJECT, entry);
            mem_deref(entry);
        }
    }
    return index;
}
void forget(const std::string &id) {
    connected_identity.erase(id);
    connected_name.erase(id);
}
void close() {
    connected_identity.clear();
    connected_name.clear();
}
} // namespace calls
