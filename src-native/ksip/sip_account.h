// The SIP account: the user agent this module registers, where it registers
// (the registrar, the transport, the media encryption), and how the
// registration stands. It owns the credential handling of the login and the
// register events; nothing else touches the user agent's registration.
//
// Threads: baresip's main thread only. Lifetime: module-static state;
// init() is the first of the ksip parts and close() the last (ksip.cpp),
// since the others read the account and the registration through it.
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
void on_event(bevent_ev ev, const bevent *e);
// A REGISTER going out (from the SIP trace): what identifies its
// transaction and where it goes, so that only its answer is taken.
void on_sent(const uint8_t *packet, size_t length, enum sip_transp tp, const sa *dst);
// A SIP answer received: when it is the 2xx to the REGISTER sent, the
// transport that carried the registration, and the registrar the keepalives
// go to (where that REGISTER went), are taken from it, since the register
// events carry no message.
void on_answer(const uint8_t *packet, size_t length, enum sip_transp tp, const sa *src);
void write_state(odict *od);
void init();
void close();
} // namespace sip_account
