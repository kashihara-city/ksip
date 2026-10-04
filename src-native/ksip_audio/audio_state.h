// The audio module's state, as the engine's state report gives it: whether
// the module came up and with any processing on, where the current call's
// microphone comes from, whether a player has the speaker and it is up, on
// which endpoint each side's call stream is and whether that stands in for
// the device asked for, and for each side the device starts that failed.
// Values of how things stand, so that a report taken at any time is right,
// however many reports were missed or whatever the log says; and a count of
// the failures for each side on its own, so that one between two reports is
// still seen and one of the speaker is not hidden by a later one of the
// microphone. The app reads nothing of this from the log.
//
// Written by the ksip_audio module (device_module.cpp, the session core's
// own records) on baresip's main thread; read there by the ksip module's
// ksip_state and by the module's own ksip_audio_state command.
#pragma once
struct odict;
// Adds to `od`:
//   "audio": {"ready": bool, "processing": bool,
//             "microphone": {"input": "device" | "silence" | "none",
//                            "endpoint": id, "stand_in": bool,
//                            "failures": n, "last_result": n},
//             "speaker": {"playing": bool, "endpoint": id, "stand_in": bool,
//                         "failures": n, "last_result": n},
//             "alert": {"playing": bool, "endpoint": id, "stand_in": bool,
//                       "failures": n, "last_result": n},
//             "capture_raw": bool, "playout_raw": bool}
// "ready" is false when the module is not up (not loaded, or its bridge
// would not start); "processing" is echo cancellation, the high-pass filter,
// the noise suppression or the AGC, any of them on. "playing" is a player
// with the stream and the stream up: an owner whose start failed is not
// playing. "endpoint" and "stand_in" are there while the side's call stream
// is up ("input" is "device", "playing" is true): the endpoint the stream
// opened, as WebRTC says, and whether it is another than the device asked for
// (the default in its place, while that device is not there or would not
// start; never for "default" asked for). "failures" counts every start of
// that side that failed, whichever path tried it (a new stream, the old
// player put back, the stream handed back, the default in a device's place);
// "last_result" is the bridge's result for the last of them, there once
// "failures" is not 0. "alert" is the same for the alert sounds (the
// ringtone), which play through their own player (alert_player.h): "playing"
// while a sound is up, the endpoint it is on, and the starts that failed. "capture_raw" and "playout_raw" are there once a
// capture or a playout stream has been opened: true when it took RAW mode,
// false when it was not asked for (ksip_raw_microphone, ksip_raw_speaker) or
// the device refused it, and the device's effects (APOs) process that audio.
extern "C" int ksip_audio_add_state(struct odict *od);
