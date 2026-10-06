// Which call is being recorded; see recording_session.h.
#include "recording_session.h"
#include "recorder.h"
#include <cstring>
#include <exception>
#include <memory>
#include <mutex>
#include <array>
#include <string>
#include <unordered_map>

namespace recording_session {
namespace {
// Every entry here is taken under the gate; the audio threads take it for
// every frame, so nothing slow happens under it: a Recorder (a file, a
// writer thread, its buffers) is made before the gate is taken and only put
// in place under it, and finished after it is let go. The commands and the
// decoder events all come on baresip's main thread, so between a command's
// two takes of the gate only the audio threads run, and they change nothing
// here. The Recorder's own lock, under the gate, is the only nesting.
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

// The file, its header and the writer: outside the gate. Null when the file
// cannot be made.
std::unique_ptr<Recorder> prepare(const std::string &path, uint32_t rate) {
    try {
        auto r = std::make_unique<Recorder>(path, rate);
        if (r->opened()) return r;
    } catch (const std::exception &e) {
        // The writer thread or its buffers could not be made: as good as a
        // file that cannot be opened, with the reason in the log.
        warning("ksip: recording: %s\n", e.what());
    }
    return nullptr;
}
// A prepared recorder becomes the recording of `stream`: under the gate,
// nothing but a few assignments. The app hears that the file is being
// written (a start it asked for answers the same; a reservation that came
// true has only this to tell it).
void install(std::unique_ptr<Recorder> r, const audio *stream) {
    const std::string path = r->path();
    current = std::move(r);
    recording_audio = stream;
    detached_audio = nullptr;
    module_event("ksip_audio_filter", "recording", nullptr, nullptr, "active %s", path.c_str());
}
// Finishing (joining the writer, closing the file) can wait on the disk, so
// it happens outside the gate; the words are the ones the app reads.
Recorder::Summary end(std::unique_ptr<Recorder> finished) {
    auto summary = finished->finish();
    info("ksip_audio_filter: WAV closed (%llu bytes, %llu dropped samples, error=%d)\n", summary.bytes, summary.dropped, summary.failed);
    // The app's word that the file is closed, whichever way that came
    // about, and whether it is whole; the stop command answers the same.
    module_event("ksip_audio_filter", "recording", nullptr, nullptr, "closed %s %llu %llu %s", summary.failed || summary.dropped ? "incomplete" : "complete",
                 summary.bytes, summary.dropped, finished->path().c_str());
    return summary;
}
bool readable(const auframe *f) { return f->fmt == AUFMT_S16LE || f->fmt == AUFMT_FLOAT; }
// One channel of the frame, as 16-bit samples, into `out`; a frame with
// more channels gives its first. Returns the count. The buffer is one fixed
// array per audio thread, so nothing is ever allocated for a frame; a frame
// longer than it (170 ms at 48 kHz, longer than any packet time baresip
// uses) is cut to it.
using Mono = std::array<int16_t, 8192>;
size_t mono(const auframe *f, Mono &out) {
    const size_t stride = std::max<size_t>(f->ch, 1), frames = std::min(f->sampc / stride, out.size());
    for (size_t i = 0; i < frames; ++i) {
        size_t at = i * stride;
        if (f->fmt == AUFMT_S16LE) out[i] = ((int16_t *)f->sampv)[at];
        else out[i] = (int16_t)std::clamp(((float *)f->sampv)[at] * 32768.f, -32768.f, 32767.f);
    }
    return frames;
}
} // namespace

void decoder_created(const audio *stream, uint32_t rate) {
    // A reserved file to open for this call, decided under the gate and
    // opened outside it.
    std::string path;
    {
        std::lock_guard<std::mutex> lock(gate);
        decoders[stream] = rate;
        if (current && !recording_audio && detached_audio == stream) {
            recording_audio = stream;
            detached_audio = nullptr;
            info("ksip_audio_filter: recording goes on with the call's new decoder\n");
        }
        if (!reserved_call.empty()) {
            auto c = uag_call_find(reserved_call.c_str());
            if (c && call_audio(c) == stream) {
                if (reserved_path.empty()) {
                    recording_audio = stream;
                    reserved_call.clear();
                    info("ksip_audio_filter: recording bound to the reserved call\n");
                } else path = reserved_path;
            }
        }
    }
    if (path.empty()) return;
    auto r = prepare(path, rate);
    std::lock_guard<std::mutex> lock(gate);
    if (r) {
        install(std::move(r), stream);
        info("ksip_audio_filter: recording bound to the reserved call\n");
    } else {
        // The app was told the recording was reserved; it has to hear that
        // no file came of it.
        warning("ksip_audio_filter: cannot open the reserved WAV file\n");
        module_event("ksip_audio_filter", "recording", nullptr, nullptr, "failed %s", path.c_str());
    }
    reserved_call.clear();
    reserved_path.clear();
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
    thread_local Mono samples;
    std::lock_guard<std::mutex> lock(gate);
    if (current && recording_audio == stream) current->push_far(samples.data(), mono(frame, samples), frame->srate);
}
void near_frame(const audio *stream, const auframe *frame) {
    if (!readable(frame)) return;
    thread_local Mono samples;
    std::lock_guard<std::mutex> lock(gate);
    if (current && recording_audio == stream) current->push_near(samples.data(), mono(frame, samples), frame->srate);
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
    uint32_t rate = 0;
    {
        std::lock_guard<std::mutex> lock(gate);
        if (current || !reserved_path.empty()) return EALREADY;
        if (!target && decoders.size() == 1) target = decoders.begin()->first;
        auto found = decoders.find(target);
        if (found == decoders.end()) {
            if (id.empty()) return re_hprintf(pf, "Call audio is not active\n"), EAGAIN;
            reserved_call = id;
            reserved_path = path;
            return re_hprintf(pf, "Recording reserved\n");
        }
        rate = found->second;
    }
    auto r = prepare(path, rate);
    if (!r) return re_hprintf(pf, "Cannot open WAV file\n"), EIO;
    std::lock_guard<std::mutex> lock(gate);
    install(std::move(r), target);
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
    return re_hprintf(pf, "Recording stopped\n");
}
int select(re_printf *pf, const char *prm) {
    if (!str_isset(prm)) return EINVAL;
    const audio *target = nullptr;
    uint32_t rate = 0;
    std::string path;
    {
        std::lock_guard<std::mutex> lock(gate);
        if (!current && reserved_path.empty()) return ENOENT;
        detached_audio = nullptr;
        if (strcmp(prm, "-") == 0) {
            recording_audio = nullptr;
            reserved_call.clear();
            return re_hprintf(pf, "Recording input paused\n");
        }
        auto c = uag_call_find(prm);
        if (!c) return ENOENT;
        target = call_audio(c);
        auto found = decoders.find(target);
        if (found == decoders.end()) {
            // The call exists but is not carrying audio yet; bind it once it does.
            recording_audio = nullptr;
            reserved_call = prm;
            return re_hprintf(pf, "Recording input reserved\n");
        }
        if (reserved_path.empty()) {
            recording_audio = target;
            reserved_call.clear();
            return re_hprintf(pf, "Recording input switched\n");
        }
        // The reserved file is for this call: opened outside the gate.
        path = reserved_path;
        rate = found->second;
    }
    auto r = prepare(path, rate);
    if (!r) return re_hprintf(pf, "Cannot open WAV file\n"), EIO;
    std::lock_guard<std::mutex> lock(gate);
    install(std::move(r), target);
    reserved_path.clear();
    reserved_call.clear();
    return re_hprintf(pf, "Recording input switched\n");
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
