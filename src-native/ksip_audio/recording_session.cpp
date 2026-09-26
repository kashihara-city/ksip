// Which call is being recorded; see recording_session.h.
#include "recording_session.h"
#include "recorder.h"
#include <cstring>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>
#include <vector>

namespace recording_session {
namespace {
// Every entry here is taken under the gate; the audio threads take it for
// every frame, so nothing slow happens under it.
std::mutex gate;
std::unique_ptr<Recorder> current;
// The calls carrying audio, with the rate their decoder runs at.
std::unordered_map<const audio *, uint32_t> decoders;
const audio *recording_audio = nullptr;
// The call whose decode filter was last taken away while it was being
// recorded. baresip flushes and remakes the receive filters of a call when
// the far end changes codec in the middle of it (a PBX that answers with
// PCMU and then sends Opus does that on every call); the same audio object
// comes straight back, and the recording must go on with it. A call that
// ended does not come back, and Rust stops or moves the recording then.
const audio *detached_audio = nullptr;
// A call reports ESTABLISHED before its decode filter exists, so a requested
// target is held here and bound as soon as the filter appears.
std::string reserved_call, reserved_path;

bool begin(const std::string &path, uint32_t rate, const audio *stream) {
    try {
        auto r = std::make_unique<Recorder>(path, rate);
        if (!r->opened()) return false;
        current = std::move(r);
    } catch (...) {
        return false;
    }
    recording_audio = stream;
    detached_audio = nullptr;
    return true;
}
// Finishing (joining the writer, closing the file) can wait on the disk, so
// it happens outside the gate; the words are the ones the app reads.
Recorder::Summary end(std::unique_ptr<Recorder> finished) {
    auto summary = finished->finish();
    info("postlab: receive WAV closed (%llu bytes, %llu dropped samples, error=%d)\n", summary.bytes, summary.dropped, summary.failed);
    return summary;
}
bool readable(const auframe *f) { return f->fmt == AUFMT_S16LE || f->fmt == AUFMT_FLOAT; }
// One channel of the frame, as 16-bit samples; a frame with more channels
// gives its first.
const std::vector<int16_t> &mono(const auframe *f) {
    // One buffer per audio thread, kept between frames: nothing is allocated
    // while a frame is being processed.
    thread_local std::vector<int16_t> out;
    const size_t stride = std::max<size_t>(f->ch, 1), frames = f->sampc / stride;
    out.resize(frames);
    for (size_t i = 0; i < frames; ++i) {
        size_t at = i * stride;
        if (f->fmt == AUFMT_S16LE) out[i] = ((int16_t *)f->sampv)[at];
        else out[i] = (int16_t)std::clamp(((float *)f->sampv)[at] * 32768.f, -32768.f, 32767.f);
    }
    return out;
}
} // namespace

void decoder_created(const audio *stream, uint32_t rate) {
    std::lock_guard<std::mutex> lock(gate);
    decoders[stream] = rate;
    if (current && !recording_audio && detached_audio == stream) {
        recording_audio = stream;
        detached_audio = nullptr;
        info("postlab: receive recording goes on with the call's new decoder\n");
    }
    if (!reserved_call.empty()) {
        auto c = uag_call_find(reserved_call.c_str());
        if (c && call_audio(c) == stream) {
            bool ok = reserved_path.empty() ? (recording_audio = stream, true) : begin(reserved_path, rate, stream);
            if (ok) info("postlab: receive recording bound to the reserved call\n");
            else warning("postlab: cannot open the reserved WAV file\n");
            reserved_call.clear();
            reserved_path.clear();
        }
    }
}
void decoder_destroyed(const audio *stream) {
    std::lock_guard<std::mutex> lock(gate);
    decoders.erase(stream);
    // Keep the WAV session alive when a call ends. Rust may select the other
    // line next, so destroying one decoder must not split the recording.
    if (recording_audio == stream) {
        recording_audio = nullptr;
        detached_audio = stream;
    }
}
void far_frame(const audio *stream, const auframe *frame) {
    if (!readable(frame)) return;
    std::lock_guard<std::mutex> lock(gate);
    if (current && recording_audio == stream) {
        const auto &samples = mono(frame);
        current->push_far(samples.data(), samples.size());
    }
}
void near_frame(const audio *stream, const auframe *frame) {
    if (!readable(frame)) return;
    std::lock_guard<std::mutex> lock(gate);
    if (current && recording_audio == stream) {
        const auto &samples = mono(frame);
        current->push_near(samples.data(), samples.size());
    }
}
int start(re_printf *pf, const char *prm) {
    if (!str_isset(prm)) return EINVAL;
    std::string text = prm;
    auto space = text.find(' ');
    const audio *target = nullptr;
    std::string path = text, id;
    if (space != std::string::npos) {
        id = text.substr(0, space);
        auto c = uag_call_find(id.c_str());
        if (c) {
            target = call_audio(c);
            path = text.substr(space + 1);
        } else id.clear();
    }
    std::lock_guard<std::mutex> lock(gate);
    if (current || !reserved_path.empty()) return EALREADY;
    if (!target && decoders.size() == 1) target = decoders.begin()->first;
    auto found = decoders.find(target);
    if (found == decoders.end()) {
        if (id.empty()) return re_hprintf(pf, "Call audio is not active\n"), EAGAIN;
        reserved_call = id;
        reserved_path = path;
        return re_hprintf(pf, "Receive-only recording reserved\n");
    }
    if (!begin(path, found->second, target)) return re_hprintf(pf, "Cannot open WAV file\n"), EIO;
    return re_hprintf(pf, "Recording started\n");
}
int stop(re_printf *pf) {
    // Under the gate the recording is only taken out of the audio threads'
    // sight; finishing it can wait on the disk, and those threads take the
    // gate for every frame, so that happens once the gate is released.
    std::unique_ptr<Recorder> finished;
    {
        std::lock_guard<std::mutex> lock(gate);
        reserved_call.clear();
        reserved_path.clear();
        detached_audio = nullptr;
        finished = std::move(current);
        recording_audio = nullptr;
    }
    if (finished) {
        auto summary = end(std::move(finished));
        if (summary.failed || summary.dropped) {
            re_hprintf(pf, "WAV write failed or samples were dropped; recording is incomplete\n");
            return EIO;
        }
    }
    return re_hprintf(pf, "Receive-only recording stopped\n");
}
int select(re_printf *pf, const char *prm) {
    if (!str_isset(prm)) return EINVAL;
    std::lock_guard<std::mutex> lock(gate);
    if (!current && reserved_path.empty()) return ENOENT;
    detached_audio = nullptr;
    if (strcmp(prm, "-") == 0) {
        recording_audio = nullptr;
        reserved_call.clear();
        return re_hprintf(pf, "Receive recording input paused\n");
    }
    auto c = uag_call_find(prm);
    if (!c) return ENOENT;
    auto target = call_audio(c);
    auto found = decoders.find(target);
    if (found == decoders.end()) {
        // The call exists but is not carrying audio yet; bind it once it does.
        recording_audio = nullptr;
        reserved_call = prm;
        return re_hprintf(pf, "Receive recording input reserved\n");
    }
    if (!reserved_path.empty()) {
        if (!begin(reserved_path, found->second, target)) return re_hprintf(pf, "Cannot open WAV file\n"), EIO;
        reserved_path.clear();
    } else recording_audio = target;
    reserved_call.clear();
    return re_hprintf(pf, "Receive recording input switched\n");
}
void close() {
    std::unique_ptr<Recorder> finished;
    {
        std::lock_guard<std::mutex> lock(gate);
        finished = std::move(current);
        recording_audio = nullptr;
    }
    if (finished) end(std::move(finished));
}
} // namespace recording_session
