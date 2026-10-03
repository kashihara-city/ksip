// The KSIP application module: what it registers with baresip (the
// commands the app sends, the event and SIP trace handlers) and how the
// parts are connected. The account, the calls, the transfer and the
// subscriptions each own their state (account.cpp, calls.cpp,
// transfer.cpp, subscriptions.cpp); reading and writing text is ksip_io's.
#include <cmath>
#include <algorithm>
#include <re.h>
#include <rem.h>
#include <baresip.h>
#include <string>
#include "ksip_audio_bridge.h"
#include "audio_state.h"
#include "ksip_io.h"
#include "sip_account.h"
#include "calls.h"
#include "subscriptions.h"
#include "system_ports.h"
#include "transfer.h"
#include "trust_state.h"

namespace {
bool sip_message_log = false;

void sip_trace(bool tx, enum sip_transp tp, const sa *src, const sa *dst, const uint8_t *packet, size_t length, void *) {
    // A keepalive (a blank line) is no SIP message; sip_account logs its own.
    bool blank = packet && length && std::all_of(packet, packet + length, [](uint8_t c) { return c == '\r' || c == '\n'; });
    if (sip_message_log && packet && length && !blank) ksip_io::log_sip_message(tx, packet, length);
    if (tx && packet) sip_account::on_sent(packet, length, tp, dst);
    if (packet && length) transfer::on_sip(tx, packet, length);
    if (tx || !packet || length < 7) return;
    sip_account::on_answer(packet, length, tp, src);
    bool update = length >= 7 && memcmp(packet, "UPDATE ", 7) == 0;
    bool reinvite = length >= 7 && memcmp(packet, "INVITE ", 7) == 0;
    if (!update && !reinvite) return;
    auto callid = ksip_io::sip_header(packet, length, "Call-ID"), pai = ksip_io::sip_header(packet, length, "P-Asserted-Identity");
    if (callid.empty() || pai.empty() || !calls::find(callid)) return;
    pl value{pai.data(), pai.size()};
    sip_addr address{};
    if (sip_addr_decode(&address, &value) || !pl_isset(&address.uri.user)) return;
    std::string uri = "sip:" + std::string(address.uri.user.p, address.uri.user.l);
    if (pl_isset(&address.uri.host)) uri += "@" + std::string(address.uri.host.p, address.uri.host.l);
    std::string name = ksip_io::display_name(address.dname);
    calls::note_identity(callid, uri, pl_isset(&address.dname) ? &name : nullptr);
}
// The events, in the order the parts have to see them: the account first,
// then what the calls do among themselves, the transfer's steps, and last
// what a closed call means for the transfer or for the call left behind.
void event(bevent_ev ev, bevent *e, void *) {
    sip_account::on_event(ev, e);
    auto c = bevent_get_call(e);
    if (!c || !call_id(c)) return;
    std::string id = call_id(c);
    system_ports::on_event(ev, c);
    if (calls::on_event(ev, e, c, id)) return;
    transfer::on_event(ev, e, c, id);
    if (ev == BEVENT_CALL_CLOSED) {
        if (!transfer::on_call_closed(c, id, bevent_get_text(e))) calls::on_call_closed(c, id);
        calls::forget(id);
    }
}
int state(re_printf *pf, void *) {
    odict *od = nullptr, *list = nullptr, *xfer = nullptr;
    int err = odict_alloc(&od, 16);
    err |= odict_alloc(&list, 16);
    err |= odict_alloc(&xfer, 8);
    if (err) {
        mem_deref(od);
        mem_deref(list);
        mem_deref(xfer);
        return err;
    }
    sip_account::write_state(od);
    // Whether the detail log runs now: set at start, switched while running.
    odict_entry_add(od, "detail_log", ODICT_BOOL, sip_message_log);
    trust_state::write_state(od);
    ksip_audio_add_state(od);
    unsigned count = calls::write_state(od, list);
    subscriptions::write_state(od);
    transfer::write_state(xfer);
    ksip_audio_stats stats{};
    if (count && !ksip_audio_get_current_stats(&stats) && stats.flags) ksip_io::add_audio_stats(od, stats);
    odict_entry_add(od, "calls", ODICT_ARRAY, list);
    odict_entry_add(od, "transfer", ODICT_OBJECT, xfer);
    err = json_encode_odict(pf, od);
    mem_deref(xfer);
    mem_deref(list);
    mem_deref(od);
    return err;
}
int action(re_printf *pf, void *arg) {
    auto a = static_cast<cmd_arg *>(arg);
    ksip_io::ActionRequest request;
    if (!a || !ksip_io::parse_action(a->prm, request)) return EINVAL;
    return calls::action(pf, request);
}
// The detail log switched while the engine runs, the three parts that
// ksip_detail_log sets at the start: the SIP messages written out whole,
// baresip's debug level, and the audio module's device warnings. "on" or "off".
int detail_log(re_printf *pf, void *arg) {
    auto a = static_cast<cmd_arg *>(arg);
    if (!a || !a->prm) return EINVAL;
    const bool on = !str_casecmp(a->prm, "on");
    if (!on && str_casecmp(a->prm, "off")) return EINVAL;
    sip_message_log = on;
    log_enable_debug(on);
    ksip_audio_set_log_level(on);
    return re_hprintf(pf, "Detail log %s\n", on ? "on" : "off");
}
// The microphone and speaker for the calls from now on, as endpoint ids (or
// "default"), so that a change of device does not need the engine
// restarted. They are the choices as saved: which endpoint serves one is
// decided as each stream opens, the default in place of a device that is not
// there (device_selection.cc, alert_player.h). baresip reads the devices out
// of its configuration when a call's audio starts, so the configuration is
// what changes here for the calls to come; a call that is up is moved onto
// them by the audio module (ksip_audio_switch), so that a choice made during
// a call reaches it. baresip's own auplay command would
// also move the alert sounds onto the call's player, which only takes the
// call's 48 kHz, so the alert stays with its own player (ksip_alert) and
// just follows the speaker.
int audio_devices(re_printf *pf, void *arg) {
    auto a = static_cast<cmd_arg *>(arg);
    config *cfg = conf_config();
    if (!cfg) return ENOENT;
    std::string microphone, speaker;
    if (!a || !ksip_io::parse_audio_devices(a->prm, sizeof(cfg->audio.play_dev), microphone, speaker)) return EINVAL;
    str_ncpy(cfg->audio.src_dev, microphone.c_str(), sizeof(cfg->audio.src_dev));
    str_ncpy(cfg->audio.play_dev, speaker.c_str(), sizeof(cfg->audio.play_dev));
    str_ncpy(cfg->audio.alert_dev, speaker.c_str(), sizeof(cfg->audio.alert_dev));
    info("ksip: audio devices, microphone %s, speaker %s\n", microphone.c_str(), speaker.c_str());
    static const char switch_command[] = "ksip_audio_switch";
    const int err = cmd_process_long(baresip_commands(), switch_command, sizeof(switch_command) - 1, pf, nullptr);
    if (err) warning("ksip: the calls that are up could not be moved onto the devices (%m)\n", err);
    return re_hprintf(pf, "Audio devices set\n");
}
const cmd commands[] = {
    {"ksip_login", 0, 0, "Load Windows SIP credential and register", sip_account::login},
    {"ksip_state", 0, 0, "Get account and per-call state", state},
    {"ksip_action", 0, CMD_PRM, "Operate a specific call", action},
    {"ksip_parking", 0, CMD_PRM, "Watch up to fifty-four numbers through dialog-state subscriptions", subscriptions::configure},
    {"ksip_shutdown", 0, 0, "Release KSIP subscriptions before quit", subscriptions::shutdown},
    {"ksip_audio_devices", 0, CMD_PRM, "Use these microphone and speaker endpoint ids, for the calls that are up too", audio_devices},
    {"ksip_detail_log", 0, CMD_PRM, "Turn the detail log on or off while running", detail_log},
};
int init() {
    sip_account::init();
    // One switch covers both kinds of detail: the SIP messages and the debug
    // level. libre is compiled with DEBUG_LEVEL 5, so its debug lines do not
    // exist in this build and only baresip has a level left to raise.
    bool detail = false;
    conf_get_bool(conf_cur(), "ksip_detail_log", &detail);
    sip_message_log = detail;
    if (detail) log_enable_debug(true);
    transfer::init();
    subscriptions::init();
    sip_set_trace_handler(uag_sip(), sip_trace);
    int err = bevent_register(event, nullptr);
    if (!err) err = cmd_register(baresip_commands(), commands, RE_ARRAY_SIZE(commands));
    return err;
}
int close() {
    sip_set_trace_handler(uag_sip(), nullptr);
    calls::close();
    transfer::close();
    subscriptions::close();
    bevent_unregister(event);
    cmd_unregister(baresip_commands(), commands);
    sip_account::close();
    return 0;
}
} // namespace
extern "C" const struct mod_export DECL_EXPORTS(ksip) = {"ksip", "application", init, close};
