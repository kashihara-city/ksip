// The SIP account: the user agent this module registers, where it registers
// (the registrar, the transport, the media encryption), and how the
// registration stands. It owns the credential handling of the login and the
// register events; nothing else touches the user agent's registration.
#pragma once
#include <cmath>
#include <algorithm>
#include <re.h>
#include <baresip.h>
#include <string>

namespace sip_account {
// The user agent, once logged in; owned by the UA group, not this module.
ua *user_agent();
bool registered();
// host:port of the registrar, and the transport the URIs carry.
const std::string &authority();
const std::string &scheme();
// A number completed with the registrar (its `#` escaped), or a URI as it is.
std::string uri_for(const std::string &value);
// The registration as the window shows it, the transport that carried it,
// and the media encryption the account was set up with.
const std::string &registration();
const std::string &registered_transport();
const std::string &media_encryption();
// The user this account registers as, for subscriptions of its own.
const std::string &own_user();
// The ksip_login command: reads the credential and registers.
int login(re_printf *pf, void *arg);
// Takes the account off the server on purpose; the 200 OK for it must not
// register it again.
int unregister();
// The register events of the user agent.
void on_event(bevent_ev ev, bevent *e);
// A SIP answer received: the transport that carried a registration is read
// from the answer itself, since the register events carry no message.
void on_answer(const uint8_t *packet, size_t length, enum sip_transp tp);
void write_state(odict *od);
void init();
void close();
} // namespace sip_account
