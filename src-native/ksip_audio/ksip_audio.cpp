#include <algorithm>
#include <atomic>
#include <cerrno>
#include <chrono>
#include <cstdint>
#include <cstring>
#include <new>
#include <thread>
#include <vector>

#include <re.h>
#include <rem.h>
#include <baresip.h>
#include "ksip_audio_bridge.h"

struct auplay_st {
  auplay_write_h *handler; void *arg; bool started;
  // The endpoint this player asked for, so that it can take the stream back.
  char device[160];
};
struct ausrc_st {
  ausrc_read_h *handler; void *arg; bool started;
  std::atomic<bool> fallback_run; std::thread fallback_thread;
};
namespace {
constexpr uint32_t kRate = 48000;
constexpr uint8_t kChannels = 1;
constexpr size_t kFrames = 480;
// How long the streams stay open after their last user has gone. Opening a
// device takes a second or more on some hardware, and a call waits for it: the
// ACK goes out once the audio is up. The ringback tone hands its stream to the
// call, and a call that ends hands its streams to the next one within this
// time; after it the devices are closed, so nothing is held while idle.
constexpr uint64_t kLingerMs = 3000;
struct auplay *g_player;
struct ausrc *g_source;
ksip_audio *g_audio;
auplay_st *g_active_playout;
ausrc_st *g_active_source;
struct tmr g_linger;
struct tmr g_handback;
// Every player alive, oldest first. Only one at a time gets the stream, the
// newest; when it goes, the one before it gets the stream back. That is how
// the ringback tone of a call that is still ringing comes back when the call
// it was pushed aside by (one the person went back to for a moment) is put on
// hold again.
std::vector<auplay_st *> g_players;

int Render(void *arg, int16_t *samples, size_t frames) {
  auto *state = static_cast<auplay_st *>(arg);
  struct auframe frame;
  auframe_init(&frame, AUFMT_S16LE, samples, frames, kRate, kChannels);
  state->handler(&frame, state->arg);
  return 0;
}
void Capture(void *arg, const int16_t *samples, size_t frames, int64_t time_ns) {
  auto *state = static_cast<ausrc_st *>(arg);
  struct auframe frame;
  auframe_init(&frame, AUFMT_S16LE, const_cast<int16_t *>(samples), frames,
               kRate, kChannels);
  frame.timestamp = time_ns > 0 ? static_cast<uint64_t>(time_ns / 1000)
                                : tmr_jiffies_usec();
  state->handler(&frame, state->arg);
}
void StopFallback(ausrc_st *state) {
  state->fallback_run.store(false);
  if (state->fallback_thread.joinable()) state->fallback_thread.join();
}
// Frames captured ahead of a call go nowhere.
void Discard(void *, const int16_t *, size_t, int64_t) {}
void StopIdleStreams(void *) {
  if (!g_audio) return;
  if (!g_active_playout && ksip_audio_playout_running(g_audio)) ksip_audio_stop_playout(g_audio);
  if (!g_active_source && ksip_audio_recording_running(g_audio)) ksip_audio_stop_recording(g_audio);
  info("ksip_audio: idle audio streams closed\n");
}
void KeepWarm() { tmr_start(&g_linger, kLingerMs, StopIdleStreams, nullptr); }
// Runs once the event that took the active player away has been dealt with:
// a tone the menu stops in the same breath must not get the stream for an
// instant on its way out.
void HandBack(void *) {
  if (!g_audio || g_active_playout) return;
  if (!g_players.empty()) {
    auto *previous = g_players.back();
    if (ksip_audio_start_playout(g_audio, previous->device, Render, previous) == 0) {
      previous->started = true;
      g_active_playout = previous;
      info("ksip_audio: WebRTC ADM playout handed back\n");
      return;
    }
  }
  KeepWarm();
}
void PlayoutDestructor(void *arg) {
  auto *state = static_cast<auplay_st *>(arg);
  g_players.erase(std::remove(g_players.begin(), g_players.end(), state), g_players.end());
  if (state != g_active_playout) return;
  g_active_playout = nullptr;
  if (!g_audio) return;
  if (state->started) ksip_audio_detach_playout(g_audio);
  tmr_start(&g_handback, 0, HandBack, nullptr);
}
void SourceDestructor(void *arg) {
  auto *state = static_cast<ausrc_st *>(arg);
  StopFallback(state);
  if (state == g_active_source) {
    if (state->started && g_audio) ksip_audio_detach_recording(g_audio);
    g_active_source = nullptr;
    KeepWarm();
  }
  state->fallback_thread.~thread();
  state->fallback_run.~atomic();
}
// The microphone is opened as soon as the speaker is, that is while the
// ringback tone plays, so that it is up by the time the call is answered.
// What it captures until then is dropped.
void OpenMicrophoneAhead() {
  if (g_active_source || ksip_audio_recording_running(g_audio)) return;
  const struct config *cfg = conf_config();
  if (!cfg || str_cmp(cfg->audio.src_mod, "ksip_audio")) return;
  const char *device = cfg->audio.src_dev[0] ? cfg->audio.src_dev : "default";
  if (ksip_audio_start_recording(g_audio, device, Discard, nullptr) == 0)
    info("ksip_audio: microphone opened ahead of the call\n");
}
bool Valid(const struct auplay_prm *p) {
  return p->srate == kRate && p->ch == kChannels && p->fmt == AUFMT_S16LE;
}
bool Valid(const struct ausrc_prm *p) {
  return p->srate == kRate && p->ch == kChannels && p->fmt == AUFMT_S16LE;
}
int AllocatePlayout(struct auplay_st **out, const struct auplay *,
    struct auplay_prm *p, const char *device, auplay_write_h *handler,
    void *arg) {
  if (!out || !p || !handler || !g_audio) return EINVAL;
  if (!Valid(p)) return ENOTSUP;
  tmr_cancel(&g_linger);
  tmr_cancel(&g_handback);
  auplay_st *previous = nullptr;
  if (g_active_playout) {
    ksip_audio_detach_playout(g_audio);
    g_active_playout->started = false;
    previous = g_active_playout;
    g_active_playout = nullptr;
  }
  auto *state = static_cast<auplay_st *>(mem_zalloc(sizeof(auplay_st), PlayoutDestructor));
  if (!state) return ENOMEM;
  state->handler = handler; state->arg = arg;
  str_ncpy(state->device, device && device[0] ? device : "default", sizeof(state->device));
  const bool running = ksip_audio_playout_running(g_audio);
  const int result = ksip_audio_start_playout(g_audio, state->device, Render, state);
  if (result) {
    warning("ksip_audio: start playout failed (%d) for %s\n", result, state->device);
    mem_deref(state);
    // The player that was detached for a start that did not happen takes its
    // stream back, so what was playing goes on instead of falling silent.
    // Failing that too, the idle streams close in their own time.
    if (previous && ksip_audio_start_playout(g_audio, previous->device, Render, previous) == 0) {
      previous->started = true; g_active_playout = previous;
      info("ksip_audio: WebRTC ADM playout handed back after a failed start\n");
    } else {
      KeepWarm();
    }
    return ENODEV;
  }
  g_players.push_back(state);
  state->started = true; g_active_playout = state; *out = state;
  info(running && ksip_audio_playout_running(g_audio)
           ? "ksip_audio: WebRTC ADM playout taken over\n"
           : "ksip_audio: WebRTC ADM playout started\n");
  OpenMicrophoneAhead();
  return 0;
}
int AllocateSource(struct ausrc_st **out, const struct ausrc *,
    struct ausrc_prm *p, const char *device, ausrc_read_h *handler,
    ausrc_error_h *, void *arg) {
  if (!out || !p || !handler || !g_audio) return EINVAL;
  if (!Valid(p)) return ENOTSUP;
  tmr_cancel(&g_linger);
  if (g_active_source) {
    ksip_audio_detach_recording(g_audio);
    g_active_source->started = false;
    StopFallback(g_active_source);
    g_active_source = nullptr;
  }
  auto *state = static_cast<ausrc_st *>(mem_zalloc(sizeof(ausrc_st), SourceDestructor));
  if (!state) return ENOMEM;
  new (&state->fallback_run) std::atomic<bool>(false);
  new (&state->fallback_thread) std::thread();
  state->handler = handler; state->arg = arg;
  const bool running = ksip_audio_recording_running(g_audio);
  const int result = ksip_audio_start_recording(g_audio,
      device && device[0] ? device : "default", Capture, state);
  if (result) {
    warning("ksip_audio: start recording failed (%d) for %s; using silence\n",
            result, device && device[0] ? device : "default");
    state->fallback_run.store(true);
    state->fallback_thread = std::thread([state] {
      int16_t samples[kFrames] = {};
      info("ksip: microphone fallback active\n");
      auto next = std::chrono::steady_clock::now();
      while (state->fallback_run.load()) {
        Capture(state, samples, kFrames, 0);
        next += std::chrono::milliseconds(10);
        std::this_thread::sleep_until(next);
      }
    });
  } else {
    state->started = true;
    info(running && ksip_audio_recording_running(g_audio)
             ? "ksip_audio: WebRTC ADM recording taken over, APM running\n"
             : "ksip_audio: WebRTC ADM recording and APM started\n");
  }
  g_active_source = state; *out = state;
  return 0;
}
}  // namespace

extern "C" int ksip_audio_get_current_stats(ksip_audio_stats *stats) {
  if (!stats || !g_audio) return EINVAL;
  return ksip_audio_get_stats(g_audio, stats);
}

// The setting's word for the noise suppression, as the bridge counts it.
static int NoiseLevel(const char *level) {
  static const char *const names[] = {"low", "moderate", "high", "very_high"};
  for (int i = 0; i < 4; ++i)
    if (!std::strcmp(level, names[i])) return i;
  return std::strcmp(level, "off") ? 2 : -1;
}

static int module_init() {
  uint32_t delay = 20;
  bool aec = true, high_pass = true, agc = true, detail = false;
  char level[24] = "high";
  conf_get_u32(conf_cur(), "webrtc_aec_delay_ms", &delay);
  conf_get_bool(conf_cur(), "ksip_aec_enabled", &aec);
  conf_get_bool(conf_cur(), "ksip_high_pass", &high_pass);
  conf_get_str(conf_cur(), "ksip_noise_suppression", level, sizeof level);
  conf_get_bool(conf_cur(), "ksip_agc", &agc);
  conf_get_bool(conf_cur(), "ksip_detail_log", &detail);
  const ksip_audio_processing processing{aec, high_pass, NoiseLevel(level), agc};
  const bool enabled = aec || high_pass || processing.noise_suppression >= 0 || agc;
  // Set before creating, so that the device warnings of a failing start show up.
  ksip_audio_set_log_level(detail);
  const int result = ksip_audio_create(std::min(delay, 500u), &processing, &g_audio);
  if (result) return ENODEV;
  tmr_init(&g_linger);
  tmr_init(&g_handback);
  int error = ausrc_register(&g_source, baresip_ausrcl(), "ksip_audio", AllocateSource);
  error |= auplay_register(&g_player, baresip_auplayl(), "ksip_audio", AllocatePlayout);
  if (error) {
    g_source = static_cast<struct ausrc *>(mem_deref(g_source));
    g_player = static_cast<struct auplay *>(mem_deref(g_player));
    ksip_audio_destroy(g_audio); g_audio = nullptr; return error;
  }
  // The app reads "processing enabled" from this line.
  if (enabled)
    info("ksip_audio: Google WebRTC ADM + APM initialized (processing enabled:"
         " aec %s, high-pass %s, noise suppression %s, agc %s)\n",
         aec ? "on" : "off", high_pass ? "on" : "off",
         processing.noise_suppression >= 0 ? level : "off", agc ? "on" : "off");
  else
    info("ksip_audio: Google WebRTC ADM + APM initialized (processing disabled)\n");
  return 0;
}
static int module_close() {
  tmr_cancel(&g_linger);
  tmr_cancel(&g_handback);
  g_source = static_cast<struct ausrc *>(mem_deref(g_source));
  g_player = static_cast<struct auplay *>(mem_deref(g_player));
  ksip_audio_destroy(g_audio); g_audio = nullptr; return 0;
}
extern "C" const struct mod_export DECL_EXPORTS(ksip_audio) = {
  "ksip_audio", "sound", module_init, module_close};
