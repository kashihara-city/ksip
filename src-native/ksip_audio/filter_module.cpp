// The audio filter module: registers a filter on every call's send and
// receive path (the software gain, in-band DTMF, and the samples the
// recording takes)
// and the commands that start, switch and stop a recording and set the
// gain. What is recorded is the recording session's business.
#include <cmath>
#include <algorithm>
#include <re.h>
#include <rem.h>
#include <baresip.h>
#include <atomic>
#include <charconv>
#include <cstdio>
#include <cstring>
#include <string>
#include <string_view>
#include <cerrno>
#include <cstdint>
#include <system_error>
#include "recording_session.h"
#include "inband_dtmf.h"

namespace {
std::atomic<float> microphone_gain{1.f};
std::atomic<float> speaker_gain{1.f};
struct Encode {
    aufilt_enc_st base;
    const audio *stream;
};
struct Decode {
    aufilt_dec_st base;
    const audio *stream;
};
// The software gain: samples are scaled and clipped; a gain of one leaves
// the frame alone.
void amplify(const auframe *f, float gain) {
    if (gain <= 1.f) return;
    if (f->fmt == AUFMT_S16LE) {
        auto samples = static_cast<int16_t *>(f->sampv);
        for (size_t i = 0; i < f->sampc; ++i) samples[i] = static_cast<int16_t>(std::clamp(std::lrint(static_cast<float>(samples[i]) * gain), long{INT16_MIN}, long{INT16_MAX}));
    } else if (f->fmt == AUFMT_FLOAT) {
        auto samples = static_cast<float *>(f->sampv);
        for (size_t i = 0; i < f->sampc; ++i) samples[i] = std::clamp(samples[i] * gain, -1.f, 1.f);
    }
}
void destroy_decode(void *p) {
    auto d = static_cast<Decode *>(p);
    list_unlink(&d->base.le);
    recording_session::decoder_destroyed(d->stream);
}
void destroy_encode(void *p) {
    auto e = static_cast<Encode *>(p);
    list_unlink(&e->base.le);
    inband_dtmf::forget(e->stream);
}
int update_encode(aufilt_enc_st **st, void **, const aufilt *, aufilt_prm *, const audio *stream) {
    if (*st) return 0;
    auto e = static_cast<Encode *>(mem_zalloc(sizeof(Encode), destroy_encode));
    if (!e) return ENOMEM;
    e->stream = stream;
    *st = &e->base;
    return 0;
}
int process_encode(aufilt_enc_st *st, auframe *f) {
    auto stream = reinterpret_cast<Encode *>(st)->stream;
    amplify(f, microphone_gain.load(std::memory_order_relaxed));
    // After the gain, so that a tone goes at its own level; before the
    // recording, which keeps what the other side heard.
    inband_dtmf::fill(stream, f);
    recording_session::near_frame(stream, f);
    return 0;
}
int update_decode(aufilt_dec_st **st, void **, const aufilt *, aufilt_prm *p, const audio *stream) {
    if (*st) return 0;
    auto d = static_cast<Decode *>(mem_zalloc(sizeof(Decode), destroy_decode));
    if (!d) return ENOMEM;
    d->stream = stream;
    recording_session::decoder_created(stream, p->srate);
    *st = &d->base;
    return 0;
}
int process_decode(aufilt_dec_st *st, auframe *f) {
    recording_session::far_frame(reinterpret_cast<Decode *>(st)->stream, f);
    amplify(f, speaker_gain.load(std::memory_order_relaxed));
    return 0;
}
int start(re_printf *pf, void *arg) {
    auto a = static_cast<cmd_arg *>(arg);
    return recording_session::start(pf, a ? a->prm : nullptr);
}
int stop(re_printf *pf, void *) { return recording_session::stop(pf); }
int select_recording(re_printf *pf, void *arg) {
    auto a = static_cast<cmd_arg *>(arg);
    return recording_session::select(pf, a ? a->prm : nullptr);
}
// The software gain as the app sets it: a percentage, from 100 (as is) to 200.
constexpr uint32_t kMinGainPercent = 100, kMaxGainPercent = 200;
constexpr float kPercent = 100.f;
int gain(re_printf *pf, void *arg) {
    auto a = static_cast<cmd_arg *>(arg);
    if (!a || !str_isset(a->prm)) return EINVAL;
    // "<kind> <level>": the level a percentage from 100 to 200 with nothing
    // after it, read strictly (sscanf would take a sign and overflow quietly).
    const std::string_view prm(a->prm);
    const size_t space = prm.find(' ');
    if (space == std::string_view::npos) return EINVAL;
    const std::string kind(prm.substr(0, space));
    const std::string_view number = prm.substr(space + 1);
    unsigned level = 0;
    const auto [end, ec] = std::from_chars(number.data(), number.data() + number.size(), level);
    if (ec != std::errc() || end != number.data() + number.size() || level < kMinGainPercent || level > kMaxGainPercent) return EINVAL;
    const auto value = static_cast<float>(level) / kPercent;
    if (kind == "microphone") microphone_gain.store(value, std::memory_order_relaxed);
    else if (kind == "speaker") speaker_gain.store(value, std::memory_order_relaxed);
    else return EINVAL;
    return re_hprintf(pf, "%s software gain %u%%\n", kind.c_str(), level);
}
aufilt filter = {};
const cmd commands[] = {
    {"ksip_record", 0, CMD_PRM, "Record a call to a WAV, the far end left and this side right", start},
    {"ksip_record_select", 0, CMD_PRM, "Select the call the current WAV follows", select_recording},
    {"ksip_record_stop", 0, 0, "Finish the WAV; an error means it is incomplete", stop},
    {"ksip_gain", 0, CMD_PRM, "Set microphone/speaker software gain", gain},
};
int init() {
    uint32_t mic = kMinGainPercent, speaker = kMinGainPercent;
    (void)conf_get_u32(conf_cur(), "ksip_microphone_gain", &mic);
    (void)conf_get_u32(conf_cur(), "ksip_speaker_gain", &speaker);
    microphone_gain.store(static_cast<float>(std::clamp(mic, kMinGainPercent, kMaxGainPercent)) / kPercent);
    speaker_gain.store(static_cast<float>(std::clamp(speaker, kMinGainPercent, kMaxGainPercent)) / kPercent);
    filter.name = "ksip_audio_filter";
    filter.encupdh = update_encode;
    filter.ench = process_encode;
    filter.decupdh = update_decode;
    filter.dech = process_decode;
    aufilt_register(baresip_aufiltl(), &filter);
    return cmd_register(baresip_commands(), commands, RE_ARRAY_SIZE(commands));
}
int close() {
    cmd_unregister(baresip_commands(), commands);
    aufilt_unregister(&filter);
    recording_session::close();
    return 0;
}
} // namespace
extern "C" const struct mod_export DECL_EXPORTS(ksip_audio_filter) = {"ksip_audio_filter", "aufilt", init, close};
