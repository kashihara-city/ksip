// Who has the speaker and the microphone, and for how long: the newest
// player gets the stream and hands it back when it goes, a start that
// fails gives the stream back to the player it took it from, a microphone
// that will not open is replaced by timed silence, and streams nobody uses
// close after a while. The device module registers with baresip and asks
// here; the streams themselves are the WebRTC bridge's.
//
// Threads: open, close and the allocations run on baresip's main thread, as
// do the destructors of a player or a source and the timers; the bridge's
// render and capture callbacks run on WebRTC's audio threads and only ever
// see the player or source they were installed with, through the bridge's
// CallbackGate. A player or source is therefore detached from the bridge
// (which drains a callback in flight) before it is freed; the fallback
// thread of a source that could not open its microphone is joined when the
// fallback stops, before the source is freed. Lifetime: one bridge for the
// module's lifetime, open() at module load and close() at unload; not
// re-opened.
#pragma once
#include <cmath>
#include <algorithm>
#include <re.h>
#include <rem.h>
#include <baresip.h>
#include <atomic>
#include <string>
#include <thread>
#include "ksip_audio_bridge.h"
#include "session_core.h"

// baresip's own names for a player and a source; declared by it, defined here.
struct auplay_st {
    auplay_write_h *handler;
    void *arg;
    bool started;
    // The endpoint this player asked for, so that it can take the stream back.
    char device_name[160];
    const char *device() const { return device_name; }
    void set_device(const char *device) { str_ncpy(device_name, device && device[0] ? device : "default", sizeof(device_name)); }
};
struct ausrc_st {
    ausrc_read_h *handler;
    void *arg;
    bool started;
    std::atomic<bool> fallback_run;
    std::thread fallback_thread;
};

namespace playback_session {
constexpr uint32_t kRate = 48000;
constexpr uint8_t kChannels = 1;
constexpr size_t kFrames = 480;
// The bridge the session works through, for the module's lifetime.
void open(ksip_audio *bridge);
void close();
ksip_audio *bridge();
// A new player or source, parameters already checked: it takes the stream
// from whoever had it.
int allocate_playout(auplay_st **out, const char *device, auplay_write_h *handler, void *arg);
int allocate_source(ausrc_st **out, const char *device, ausrc_read_h *handler, void *arg);
// The devices chosen again while a call is up, taken by the streams that
// are up now (session_core.h's switch_devices); null leaves a side alone.
// What came of each side is returned.
Switch switch_devices(const char *microphone, const char *speaker);
// The session's part of the module's state (audio_state.h): "microphone"
// and "speaker", added to `audio`.
void add_state(odict *audio);
// The playout side of a core as audio_state.h writes a side: "playing",
// "endpoint" and "stand_in" while up, "failures" and "last_result"; for the
// call's speaker (the session's core) and for the alert sounds' (alert_player.h).
template <class AnyCore>
void add_playout_state(odict *audio, const char *name, AnyCore &core) {
    odict *side = nullptr;
    if (odict_alloc(&side, 8)) return;
    odict_entry_add(side, "playing", ODICT_BOOL, core.playing());
    const std::string endpoint = core.endpoint(true);
    if (!endpoint.empty()) {
        odict_entry_add(side, "endpoint", ODICT_STRING, endpoint.c_str());
        odict_entry_add(side, "stand_in", ODICT_BOOL, core.stand_in(true));
    }
    const auto failures = core.speaker_failures();
    odict_entry_add(side, "failures", ODICT_INT, static_cast<int64_t>(failures.count));
    if (failures.count) odict_entry_add(side, "last_result", ODICT_INT, static_cast<int64_t>(failures.last_result));
    odict_entry_add(audio, name, ODICT_OBJECT, side);
    mem_deref(side);
}
} // namespace playback_session
