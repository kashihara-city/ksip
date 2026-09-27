// The audio module's state, as the engine's state report gives it: whether
// the module came up and with any processing on, where the current call's
// microphone comes from, whether a player has the speaker, and the device
// starts that failed. Values of how things stand, so that a report taken at
// any time is right, however many reports were missed or whatever the log
// says; and a count for the failures, so that one between two reports is
// still seen. The app reads nothing of this from the log.
//
// Written by the ksip_audio module (device_module.cpp, the session core's
// own records) on baresip's main thread; read there by the ksip module's
// ksip_state and by the module's own ksip_audio_state command.
#pragma once
struct odict;
// Adds to `od`:
//   "audio": {"ready": bool, "processing": bool,
//             "input": "device" | "silence" | "none", "output": bool,
//             "failures": n, "last_failure": {"side": "speaker" | "microphone", "result": n}}
// "ready" is false when the module is not up (not loaded, or its bridge
// would not start); "processing" is echo cancellation, the high-pass filter,
// the noise suppression or the AGC, any of them on; "last_failure" is there
// once "failures" is not 0, and its "result" is the bridge's start result.
extern "C" int ksip_audio_add_state(struct odict *od);
