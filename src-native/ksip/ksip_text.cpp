// The module's reading of plain text; see ksip_text.h.
#include "ksip_text.h"
#include <cctype>
#include <cstring>
#include <vector>

namespace ksip_text {
namespace {
std::string lower(std::string s) {
    for (auto &ch : s) ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    return s;
}
std::string trim(std::string s) {
    auto first = s.find_first_not_of(" \t\r\n"), last = s.find_last_not_of(" \t\r\n");
    return first == std::string::npos ? std::string() : s.substr(first, last - first + 1);
}
// The name in front of a header's colon, in lower case and without the white
// space HCOLON allows around it (RFC 3261 25.1: `HCOLON = *( SP / HTAB ) ":" SWS`).
std::string header_name(const std::string &line, size_t colon) {
    size_t end = colon;
    while (end > 0 && (line[end - 1] == ' ' || line[end - 1] == '\t')) --end;
    return lower(line.substr(0, end));
}
} // namespace

bool sip_uri(const std::string &s) {
    std::string head = lower(s.substr(0, 5));
    return head.rfind("sip:", 0) == 0 || head.rfind("sips:", 0) == 0;
}
bool address_ok(const std::string &s) {
    if (s.empty() || s.size() > 200) return false;
    for (unsigned char ch : s) if (ch < 0x21 || ch > 0x7e) return false;
    return true;
}
bool token(const char *s, const char *extra) {
    if (!s || !*s || strlen(s) > 253) return false;
    for (; *s; ++s)
        if (!(static_cast<unsigned char>(*s) < 128 && (isalnum(static_cast<unsigned char>(*s)) || strchr(extra, *s)))) return false;
    return true;
}
std::string sip_header(const uint8_t *packet, size_t length, const char *name) {
    const std::string wanted = lower(name);
    // The lines of the head, each without its CRLF, up to the blank line.
    std::vector<std::string> lines;
    size_t pos = 0;
    while (pos < length) {
        size_t end = pos;
        while (end + 1 < length && !(packet[end] == '\r' && packet[end + 1] == '\n')) ++end;
        if (end == pos) break;
        lines.emplace_back(reinterpret_cast<const char *>(packet + pos), end - pos);
        if (end + 1 >= length) break;
        pos = end + 2;
    }
    auto folded = [](const std::string &line) { return !line.empty() && (line[0] == ' ' || line[0] == '\t'); };
    // The first line (the request or status line) and the continuation lines
    // of a header are never taken for a header of their own.
    for (size_t i = 1; i < lines.size(); ++i) {
        if (folded(lines[i])) continue;
        auto colon = lines[i].find(':');
        if (colon == std::string::npos || header_name(lines[i], colon) != wanted) continue;
        // A value folded over several lines (RFC 3261 7.3.1): each line that
        // starts with white space continues it, and the fold is one space.
        std::string value = lines[i].substr(colon + 1);
        for (size_t j = i + 1; j < lines.size() && folded(lines[j]); ++j) value += ' ' + lines[j];
        size_t begin = 0;
        while (begin < value.size() && (value[begin] == ' ' || value[begin] == '\t')) ++begin;
        size_t stop = value.size();
        while (stop > begin && (value[stop - 1] == ' ' || value[stop - 1] == '\t')) --stop;
        std::string out;
        // The white space of the fold, and around it, reads as one space.
        for (size_t k = begin; k < stop; ++k) {
            const bool space = value[k] == ' ' || value[k] == '\t';
            if (space && !out.empty() && out.back() == ' ') continue;
            out += space ? ' ' : value[k];
        }
        return out;
    }
    return {};
}
namespace {
// A header by its name or its compact form (RFC 3261 7.3.3).
std::string header_or_compact(const uint8_t *packet, size_t length, const char *name, const char *compact) {
    auto value = sip_header(packet, length, name);
    return value.empty() ? sip_header(packet, length, compact) : value;
}
std::string evened(const std::string &s) {
    std::string out;
    for (char c : s) {
        if (c == ' ' || c == '\t') {
            if (!out.empty() && out.back() != ' ') out += ' ';
        } else {
            out += c;
        }
    }
    while (!out.empty() && out.back() == ' ') out.pop_back();
    return out;
}
// The branch parameter of a Via value (the first Via, when several are on one line).
std::string via_branch(const std::string &via) {
    const std::string first = via.substr(0, via.find(','));
    const std::string low = lower(first);
    size_t at = 0;
    while ((at = low.find("branch", at)) != std::string::npos) {
        size_t eq = at + 6;
        while (eq < low.size() && (low[eq] == ' ' || low[eq] == '\t')) ++eq;
        if (at > 0 && (low[at - 1] == ';' || low[at - 1] == ' ' || low[at - 1] == '\t') && eq < low.size() && low[eq] == '=') {
            size_t begin = eq + 1;
            while (begin < first.size() && (first[begin] == ' ' || first[begin] == '\t')) ++begin;
            size_t end = begin;
            while (end < first.size() && first[end] != ';' && first[end] != ' ' && first[end] != '\t') ++end;
            return first.substr(begin, end - begin);
        }
        at += 6;
    }
    return {};
}
} // namespace
TransactionIds request_ids(const uint8_t *packet, size_t length, const char *method) {
    const size_t n = strlen(method);
    if (!packet || length <= n || memcmp(packet, method, n) != 0 || packet[n] != ' ') return {};
    TransactionIds ids{header_or_compact(packet, length, "Call-ID", "i"), evened(sip_header(packet, length, "CSeq")),
                       via_branch(header_or_compact(packet, length, "Via", "v"))};
    if (ids.cseq.empty() || ids.branch.empty()) return {};
    return ids;
}
bool is_success_answer(const TransactionIds &request, const uint8_t *packet, size_t length) {
    if (request.call_id.empty() || !packet || length < 9 || memcmp(packet, "SIP/2.0 2", 9) != 0) return false;
    return header_or_compact(packet, length, "Call-ID", "i") == request.call_id &&
           evened(sip_header(packet, length, "CSeq")) == request.cseq &&
           via_branch(header_or_compact(packet, length, "Via", "v")) == request.branch;
}
std::vector<std::string> scrubbed_sip_lines(const uint8_t *packet, size_t length) {
    // Secrets never reach the log: digest headers are replaced before printing,
    // together with any folded continuation lines of theirs (RFC 3261 7.3.1),
    // and so are the SRTP keys an SDP body carries in a=crypto (RFC 4568),
    // which SDES and OSRTP put in the signalling. The file this goes to
    // outlives the call, and a key in it would unlock a captured recording.
    static const char *secrets[] = {"authorization", "proxy-authorization", "www-authenticate", "proxy-authenticate"};
    std::string text(reinterpret_cast<const char *>(packet), length);
    std::vector<std::string> lines;
    bool hiding = false;
    for (size_t pos = 0; pos < text.size();) {
        size_t end = text.find("\r\n", pos);
        if (end == std::string::npos) end = text.size();
        std::string line = text.substr(pos, end - pos);
        pos = end + 2;
        if (!line.empty() && (line[0] == ' ' || line[0] == '\t')) {
            if (hiding) continue;
        } else hiding = false;
        auto colon = line.find(':');
        if (colon != std::string::npos) {
            std::string name = header_name(line, colon);
            for (auto secret : secrets)
                if (name == secret) {
                    line = line.substr(0, colon) + ": ***";
                    hiding = true;
                    break;
                }
        }
        if (line.compare(0, 9, "a=crypto:") == 0) {
            auto key = line.find(" inline:");
            if (key != std::string::npos) line = line.substr(0, key) + " inline:***";
        }
        if (!line.empty()) lines.push_back(line);
    }
    return lines;
}
DialogInfo read_dialog_info(const std::string &body) {
    DialogInfo info;
    // An attribute's value, with either quote and any spacing around `=`.
    auto attribute = [&](const std::string &attrs, const char *name) {
        const std::string wanted = lower(name);
        size_t pos = 0;
        while (pos < attrs.size()) {
            size_t start = attrs.find_first_not_of(" \t\r\n", pos);
            if (start == std::string::npos) break;
            size_t eq = attrs.find('=', start);
            if (eq == std::string::npos) break;
            std::string key = lower(trim(attrs.substr(start, eq - start)));
            size_t quote = attrs.find_first_not_of(" \t\r\n", eq + 1);
            if (quote == std::string::npos || (attrs[quote] != '"' && attrs[quote] != '\'')) break;
            size_t close = attrs.find(attrs[quote], quote + 1);
            if (close == std::string::npos) break;
            if (key == wanted) return attrs.substr(quote + 1, close - quote - 1);
            pos = close + 1;
        }
        return std::string();
    };
    // Nothing counts until the whole body has passed: what is gathered here
    // becomes the reading only at the end.
    auto nothing = [&] { return DialogInfo{}; };
    auto known_state = [](const std::string &s) {
        return s == "trying" || s == "proceeding" || s == "early" || s == "confirmed" || s == "terminated";
    };
    std::vector<std::string> open;
    std::string dialog_id;
    bool dialog_has_state = false, seen_info = false, closed_info = false;
    for (size_t pos = body.find('<'); pos != std::string::npos; pos = body.find('<', pos)) {
        auto end = body.find('>', pos);
        if (end == std::string::npos) return nothing();
        std::string tag = body.substr(pos + 1, end - pos - 1);
        pos = end + 1;
        if (tag.empty()) return nothing();
        if (tag[0] == '?' || tag[0] == '!') continue;
        if (closed_info) return nothing(); // an element after the report's end
        bool closing = tag[0] == '/';
        if (closing) tag.erase(0, 1);
        tag = trim(tag);
        bool self_closing = !closing && !tag.empty() && tag.back() == '/';
        if (self_closing) {
            tag.pop_back();
            tag = trim(tag);
        }
        auto space = tag.find_first_of(" \t\r\n");
        std::string name = lower(tag.substr(0, space)), attrs = space == std::string::npos ? std::string() : tag.substr(space);
        auto colon = name.find(':');
        if (colon != std::string::npos) name.erase(0, colon + 1);
        if (name.empty()) return nothing();
        if (closing) {
            // Every element closes in the order it opened.
            if (open.empty() || open.back() != name) return nothing();
            open.pop_back();
            if (name == "dialog") {
                if (!dialog_has_state) return nothing();
            } else if (name == "dialog-info") closed_info = true;
            continue;
        }
        if (open.empty()) {
            // The root is dialog-info, once, and says whether it is full or partial.
            if (name != "dialog-info" || seen_info) return nothing();
            seen_info = true;
            std::string state = lower(attribute(attrs, "state"));
            if (state != "full" && state != "partial") return nothing();
            info.partial = state == "partial";
            if (self_closing) closed_info = true;
            else open.push_back(name);
            continue;
        }
        if (name == "dialog") {
            // A dialog sits right under dialog-info, with an id and a state.
            if (open.back() != "dialog-info" || self_closing) return nothing();
            dialog_id = attribute(attrs, "id");
            if (dialog_id.empty()) return nothing();
            dialog_has_state = false;
            open.push_back(name);
        } else if (name == "state" && open.back() == "dialog") {
            if (self_closing || dialog_has_state) return nothing();
            auto text_end = body.find('<', pos);
            if (text_end == std::string::npos) return nothing();
            std::string text = lower(trim(body.substr(pos, text_end - pos)));
            if (!known_state(text)) return nothing();
            info.dialogs.emplace_back(dialog_id, text);
            dialog_has_state = true;
            open.push_back(name);
        } else if (!self_closing) {
            // Other elements (local, remote, their children) are passed over,
            // but they too must close in order.
            open.push_back(name);
        }
    }
    if (!seen_info || !closed_info || !open.empty()) return nothing();
    info.readable = true;
    return info;
}
std::string codec_list(const std::string &names) {
    static const std::pair<const char *, const char *> known[] = {
        {"opus", "opus/48000/1"}, {"G722", "G722/16000/1"}, {"PCMU", "PCMU/8000/1"}, {"PCMA", "PCMA/8000/1"}};
    std::string codecs;
    for (size_t start = 0; start <= names.size();) {
        auto end = names.find(',', start);
        std::string name = names.substr(start, end == std::string::npos ? std::string::npos : end - start);
        for (auto &entry : known)
            if (name == entry.first && codecs.find(entry.second) == std::string::npos) codecs += (codecs.empty() ? "" : ",") + std::string(entry.second);
        if (end == std::string::npos) break;
        start = end + 1;
    }
    if (codecs.empty()) codecs = "opus/48000/1,G722/16000/1,PCMU/8000/1,PCMA/8000/1";
    return codecs;
}
std::string escape_user(const std::string &value) {
    std::string user;
    for (char ch : value) user += ch == '#' ? "%23" : std::string(1, ch);
    return user;
}
bool parse_watch_list(const char *prm, std::array<std::string, WATCH_COUNT> &values) {
    if (!prm || !*prm) return false;
    std::string text = prm;
    size_t start = 0, count = 0;
    for (;;) {
        if (count == values.size()) return false;
        auto end = text.find(',', start);
        values[count] = text.substr(start, end == std::string::npos ? end : end - start);
        if (!values[count].empty() &&
            !(sip_uri(values[count]) ? address_ok(values[count]) : (values[count].size() <= 30 && token(values[count].c_str(), "*#+"))))
            return false;
        ++count;
        if (end == std::string::npos) break;
        start = end + 1;
    }
    for (size_t i = 0; i < values.size(); ++i)
        for (size_t j = i + 1; j < values.size(); ++j)
            if (!values[i].empty() && values[i] == values[j]) return false;
    return true;
}
bool parse_audio_devices(const char *prm, size_t limit, std::string &microphone, std::string &speaker) {
    if (!prm || !*prm) return false;
    std::string text = prm;
    auto comma = text.find(',');
    if (comma == std::string::npos) return false;
    microphone = text.substr(0, comma);
    speaker = text.substr(comma + 1);
    for (const auto &device : {microphone, speaker}) {
        if (device.empty() || device.size() >= limit) return false;
        for (unsigned char ch : device) if (ch < 0x20 || ch == 0x7f || ch == ',') return false;
    }
    return true;
}
} // namespace ksip_text
