// The audio module's state, as the engine's state report gives it: whether
// the module came up and with any processing on, where the current call's
// microphone comes from, whether a player has the speaker, and for each side
// the device starts that failed. Values of how things stand, so that a report
// taken at any time is right, however many reports were missed or whatever
// the log says; and a count of the failures for each side on its own, so
// that one between two reports is still seen and one of the speaker is not
// hidden by a later one of the microphone. The app reads nothing of this
// from the log.
//
// Written by the ksip_audio module (device_module.cpp, the session core's
// own records) on baresip's main thread; read there by the ksip module's
// ksip_state and by the module's own ksip_audio_state command.
#pragma once
struct odict;
// Adds to `od`:
//   "audio": {"ready": bool, "processing": bool,
//             "microphone": {"input": "device" | "silence" | "none", "failures": n, "last_result": n},
//             "speaker": {"playing": bool, "failures": n, "last_result": n},
//             "capture_raw": bool, "playout_raw": bool}
// "ready" is false when the module is not up (not loaded, or its bridge
// would not start); "processing" is echo cancellation, the high-pass filter,
// the noise suppression or the AGC, any of them on. "failures" counts every
// start of that side that failed, whichever path tried it (a new stream, the
// old player put back, the stream handed back); "last_result" is the
// bridge's result for the last of them, there once "failures" is not 0.
// "capture_raw" and "playout_raw" are there once a capture or a playout
// stream has been opened: true when it took RAW mode, false when it was not
// asked for (ksip_raw_microphone, ksip_raw_speaker) or the device refused it,
// and the device's effects (APOs) process that audio.
extern "C" int ksip_audio_add_state(struct odict *od);
