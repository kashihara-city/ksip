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
void on_answer(const uint8_t *packet, size_t length, enum sip_transp tp) {
    if (!unregistered && length >= 9 && memcmp(packet, "SIP/2.0 2", 9) == 0 &&
        ksip_io::sip_header(packet, length, "CSeq").find("REGISTER") != std::string::npos)
        transport_ = sip_transp_name(tp);
}
void write_state(odict *od) {
    odict_entry_add(od, "registration", ODICT_STRING, registration_.c_str());
    odict_entry_add(od, "transport", ODICT_STRING, transport_.c_str());
    odict_entry_add(od, "media_encryption", ODICT_STRING, media_encryption_.c_str());
}
void init() {
    uint32_t interval = register_interval;
    if (!conf_get_u32(conf_cur(), "ksip_register_interval", &interval) && interval >= 30 && interval <= 3600) register_interval = interval;
}
void close() { account_ua = nullptr; }
} // namespace sip_account
