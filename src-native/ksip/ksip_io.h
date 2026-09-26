// The module's reading and writing of text: SIP headers and their log form,
// the dialog-info bodies of the parking subscriptions, the command arguments
// the app sends, and the JSON the state reply is built from. Nothing here
// holds state or touches a call; it is what the control side reads with.
#pragma once
#include <re.h>
#include <baresip.h>
#include <array>
#include <cstdint>
#include <string>
#include <utility>
#include <vector>
#include "ksip_audio_bridge.h"

namespace ksip_io {
// A display name as the window shows it: without the quotes a header puts
// around it, and empty when there is none.
std::string display_name(const pl &name);
// A button may name a full SIP URI instead of a number; it is passed on as it
// is, and only has to be one line of visible ASCII.
bool sip_uri(const std::string &s);
bool address_ok(const std::string &s);
// A token of the characters SIP allows in a user or host, plus `extra`.
bool token(const char *s, const char *extra);
// The value of a header in a SIP message, or empty.
std::string sip_header(const uint8_t *packet, size_t length, const char *name);
// The lines of a SIP message as the log may show them: digest headers and
// their folded continuations, and the SRTP keys of a=crypto, are replaced.
std::vector<std::string> scrubbed_sip_lines(const uint8_t *packet, size_t length);
// Writes the scrubbed lines to the log, marked as sent or received.
void log_sip_message(bool tx, const uint8_t *packet, size_t length);
// A dialog-info body (RFC 4235) as far as the parking state needs it: whether
// it could be read as one, whether it is a full or a partial report, and the
// (id, state) of each dialog. The tags are walked, so a namespace prefix, a
// self-closing element or unusual spacing does not change the reading; a body
// that is not dialog-info at all reads as nothing, not as "free".
struct DialogInfo {
    bool readable = false;
    bool partial = false;
    std::vector<std::pair<std::string, std::string>> dialogs;
};
DialogInfo read_dialog_info(const std::string &body);
// The codecs the app chose, in its order, as the account parameter names
// them; a name this build does not know is skipped, and none at all means
// every codec in the usual order.
std::string codec_list(const std::string &names);
// A user part as it goes into a URI: `#` has to be escaped.
std::string escape_user(const std::string &value);
// What the app asks of a call: the JSON of ksip_action read into its parts.
struct ActionRequest {
    std::string op, id, value;
};
bool parse_action(const char *prm, ActionRequest &request);
// The numbers the buttons watch, comma-separated, up to the array's size;
// an empty one is a slot nobody watches. False for a number that is not a
// number or URI, for too many, or for one named twice.
bool parse_watch_list(const char *prm, std::array<std::string, 30> &values);
// The audio devices command: microphone and speaker endpoint ids, comma-separated.
bool parse_audio_devices(const char *prm, size_t limit, std::string &microphone, std::string &speaker);
// The echo canceller's statistics as the state reply carries them.
void add_audio_stats(odict *od, const ksip_audio_stats &stats);
} // namespace ksip_io
