// The SIP account; see account.h. Account credentials stay in Windows
// Credential Manager and process memory.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <wincred.h>
#include "sip_account.h"
#include "ksip_io.h"
#include "subscriptions.h"
#include <cstring>

namespace sip_account {
namespace {
ua *account_ua = nullptr; // Owned by the UA group, not this module.
std::string authority_, scheme_ = "udp", registration_ = "UNCONFIGURED";
std::string transport_, media_encryption_, own_user_;
uint32_t register_interval = 300;
// Set when this phone was taken off the server on purpose. The 200 OK for a
// de-registration arrives as a register event, which must not undo it.
bool unregistered = false;
std::string server_host_;

// Keepalives: a blank line (CRLF CRLF) to the registrar every interval, on
// the flow the registration uses (the SIP UDP socket, or the TCP/TLS
// connection), so that a firewall or NAT in between keeps the way back open
// between re-registrations. Windows Firewall let a UDP reply in for 90 but
// not 120 seconds after the last packet out where it was measured, and a
// re-registration comes every 270 of the default 300; without keepalives an
// INVITE could be dropped on its way in. Over TCP/TLS the blank line is
// RFC 5626's ping; over UDP it is in no standard, but a SIP stack skips it
// (it is no message), and the flow is kept open whatever the content.
// RFC 5626 proper is not used: libre sends its keepalives only when the
// registrar requires "outbound", which the PBXs in use do not, and at the
// registrar's interval, not ours.
bool keepalive_on = true;
uint32_t keepalive_interval = 60;
tmr keepalive_timer;
sa registrar{};
enum sip_transp registrar_tp = SIP_TRANSP_NONE;
// What has been said about the keepalives, so that the log has the first
// one and the first of each run of failures, not one line a minute.
bool keepalive_told = false;
int keepalive_failure = 0;
// The REGISTER last sent: what identifies its transaction, where it went
// and on which transport. Only an answer to it moves the keepalives.
ksip_text::TransactionIds register_sent;
sa register_dst{};
enum sip_transp register_tp = SIP_TRANSP_NONE;
} // namespace

ua *user_agent() { return account_ua; }
bool registered() { return account_ua && ua_isregistered(account_ua); }
const std::string &authority() { return authority_; }
const std::string &scheme() { return scheme_; }
std::string uri_for(const std::string &value) {
    return ksip_io::sip_uri(value) ? value : "sip:" + ksip_io::escape_user(value) + "@" + authority_ + ";transport=" + scheme_;
}
const std::string &registration() { return registration_; }
const std::string &registered_transport() { return transport_; }
const std::string &media_encryption() { return media_encryption_; }
const std::string &own_user() { return own_user_; }

namespace {
void send_keepalive(void *) {
    tmr_start(&keepalive_timer, keepalive_interval * 1000ull, send_keepalive, nullptr);
    if (!registered() || unregistered || registrar_tp == SIP_TRANSP_NONE) return;
    mbuf *mb = mbuf_alloc(4);
    if (!mb) return;
    mbuf_write_str(mb, "\r\n\r\n");
    mb->pos = 0;
    // The host is what the registration's TLS connection was verified
    // against; a connection opened again for this one is verified the same.
    std::string host = server_host_;
    int err = sip_send_conn(uag_sip(), nullptr, registrar_tp, &registrar, host.data(), mb, nullptr, nullptr);
    mem_deref(mb);
    if (err) {
        if (err != keepalive_failure)
            warning("ksip: keepalive to %J over %s could not be sent: %m\n", &registrar, sip_transp_name(registrar_tp), err);
        keepalive_failure = err;
        return;
    }
    if (keepalive_failure) info("ksip: keepalive to %J sent again\n", &registrar);
    keepalive_failure = 0;
    if (!keepalive_told)
        info("ksip: keepalive (CRLF) every %u s to %J over %s\n", keepalive_interval, &registrar, sip_transp_name(registrar_tp));
    keepalive_told = true;
}
} // namespace

int login(re_printf *pf, void *) {
    using ksip_io::token;
    if (account_ua) return EALREADY;
    // The address is policy data and arrives through the config. Only the user
    // name and its password come from the credential vault.
    char *server = nullptr, *extension = nullptr;
    uint32_t port = 0;
    pl value = PL_INIT;
    if (!conf_get(conf_cur(), "ksip_sip_server", &value) && pl_isset(&value)) pl_strdup(&server, &value);
    value = PL_INIT;
    if (!conf_get(conf_cur(), "ksip_extension", &value) && pl_isset(&value)) pl_strdup(&extension, &value);
    char *transport = nullptr, *mediaenc = nullptr;
    value = PL_INIT;
    if (!conf_get(conf_cur(), "ksip_sip_transport", &value) && pl_isset(&value)) pl_strdup(&transport, &value);
    value = PL_INIT;
    if (!conf_get(conf_cur(), "ksip_mediaenc", &value) && pl_isset(&value)) pl_strdup(&mediaenc, &value);
    conf_get_u32(conf_cur(), "ksip_sip_port", &port);
    wchar_t target[200];
    DWORD n = GetEnvironmentVariableW(L"KSIP_CREDENTIAL_TARGET", target, RE_ARRAY_SIZE(target));
    int err = (!n || n >= RE_ARRAY_SIZE(target)) ? EINVAL : 0;
    PCREDENTIALW credential = nullptr;
    if (!err && !CredReadW(target, CRED_TYPE_GENERIC, 0, &credential)) err = EACCES;
    std::string auth, password;
    if (!err) {
        if (credential->UserName) {
            int size = WideCharToMultiByte(CP_UTF8, 0, credential->UserName, -1, nullptr, 0, nullptr, nullptr);
            if (size > 1) {
                auth.resize(size - 1);
                WideCharToMultiByte(CP_UTF8, 0, credential->UserName, -1, auth.data(), size, nullptr, nullptr);
            }
        }
        if (credential->CredentialBlobSize && credential->CredentialBlobSize <= 1024)
            password.assign(reinterpret_cast<char *>(credential->CredentialBlob), credential->CredentialBlobSize);
        SecureZeroMemory(credential->CredentialBlob, credential->CredentialBlobSize);
        CredFree(credential);
    }
    // An empty extension means the account registers under its user name.
    std::string user = (extension && *extension) ? extension : auth;
    own_user_ = user;
    if (!err && (!token(server, ".-") || !token(user.c_str(), "_.+-") || !token(auth.c_str(), "_.+-") ||
                 password.empty() || password.size() > 512 || !port || port > 65535))
        err = EINVAL;
    if (!err) {
        server_host_ = server;
        authority_ = std::string(server) + ":" + std::to_string(port);
        // The transport belongs in the URI, the media encryption in the parameters.
        scheme_ = (transport && !str_casecmp(transport, "TLS")) ? "tls" : (transport && !str_casecmp(transport, "TCP")) ? "tcp" : "udp";
        // The codecs the app chose, in its order.
        pl list = PL_INIT;
        std::string names;
        if (!conf_get(conf_cur(), "ksip_audio_codecs", &list) && pl_isset(&list)) names.assign(list.p, list.l);
        std::string codecs = ksip_io::codec_list(names);
        std::string aor = "<sip:" + user + "@" + authority_ + ";transport=" + scheme_ + ">;regint=0;audio_codecs=" + codecs +
                          ";answermode=manual;call_transfer=yes";
        media_encryption_ = (mediaenc && *mediaenc) ? mediaenc : "";
        if (mediaenc && *mediaenc) aor += ";mediaenc=" + std::string(mediaenc);
        ua *created = nullptr;
        err = ua_alloc(&created, aor.c_str());
        if (!err) {
            err = account_set_auth_user(ua_account(created), auth.c_str());
            err |= account_set_auth_pass(ua_account(created), password.c_str());
            err |= account_set_regint(ua_account(created), register_interval);
            if (!err) {
                account_ua = created;
                registration_ = "REGISTERING";
                err = ua_register(created);
            }
            if (err) {
                account_ua = nullptr;
                ua_destroy(created);
                registration_ = "REGISTER_FAIL";
            }
        }
    }
    SecureZeroMemory(password.data(), password.size());
    mem_deref(server);
    mem_deref(extension);
    mem_deref(transport);
    mem_deref(mediaenc);
    if (!err) re_hprintf(pf, "Registration started\n");
    return err;
}
int unregister() {
    if (!account_ua) return EINVAL;
    unregistered = true;
    ua_unregister(account_ua);
    registration_ = "UNREGISTERED";
    transport_.clear();
    return 0;
}
void on_event(bevent_ev ev, bevent *e) {
    if (bevent_get_ua(e) != account_ua) return;
    if (ev == BEVENT_REGISTER_OK && !unregistered) {
        registration_ = "REGISTER_OK";
        subscriptions::subscribe_all();
    }
    if (ev == BEVENT_REGISTER_FAIL) {
        registration_ = "REGISTER_FAIL";
        transport_.clear();
    }
    if (ev == BEVENT_REGISTERING) registration_ = "REGISTERING";
    if (ev == BEVENT_UNREGISTERING) {
        registration_ = "UNREGISTERING";
        transport_.clear();
    }
}
void on_sent(const uint8_t *packet, size_t length, enum sip_transp tp, const sa *dst) {
    auto ids = ksip_text::request_ids(packet, length, "REGISTER");
    if (ids.call_id.empty() || !dst) return;
    register_sent = ids;
    register_dst = *dst;
    register_tp = tp;
}
void on_answer(const uint8_t *packet, size_t length, enum sip_transp tp, const sa *src) {
    // The trace sees every packet before libre matches it to a transaction,
    // so an answer counts only when it is the answer to the REGISTER sent
    // (the same Call-ID, CSeq and Via branch, which is new for each
    // transaction) on its transport: another's "200 OK" does not move the
    // keepalives. Where it came from is not asked for: a registrar may answer
    // from another address; the keepalives go where the REGISTER went.
    (void)src;
    if (unregistered || tp != register_tp || !ksip_text::is_success_answer(register_sent, packet, length)) return;
    transport_ = sip_transp_name(tp);
    // The registrar the registration went to, as the name resolved, and the
    // flow it is on. The first one starts the keepalives.
    registrar = register_dst;
    bool first = registrar_tp == SIP_TRANSP_NONE;
    registrar_tp = tp;
    if (first && keepalive_on) tmr_start(&keepalive_timer, keepalive_interval * 1000ull, send_keepalive, nullptr);
}
void write_state(odict *od) {
    odict_entry_add(od, "registration", ODICT_STRING, registration_.c_str());
    odict_entry_add(od, "transport", ODICT_STRING, transport_.c_str());
    odict_entry_add(od, "media_encryption", ODICT_STRING, media_encryption_.c_str());
}
void init() {
    uint32_t interval = register_interval;
    if (!conf_get_u32(conf_cur(), "ksip_register_interval", &interval) && interval >= 30 && interval <= 3600) register_interval = interval;
    tmr_init(&keepalive_timer);
    // The app writes both; anything it would not write keeps the defaults.
    pl mode = PL_INIT;
    if (!conf_get(conf_cur(), "ksip_keepalive", &mode) && pl_isset(&mode)) {
        if (!pl_strcasecmp(&mode, "off")) keepalive_on = false;
        else if (!pl_strcasecmp(&mode, "crlf")) keepalive_on = true;
    }
    interval = keepalive_interval;
    if (!conf_get_u32(conf_cur(), "ksip_keepalive_interval", &interval) && interval >= 10 && interval <= 600) keepalive_interval = interval;
}
void close() {
    tmr_cancel(&keepalive_timer);
    account_ua = nullptr;
}
} // namespace sip_account
