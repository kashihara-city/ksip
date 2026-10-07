// The microphone's own mute (Windows', the one the tray flyout, the app's
// mute button and a headset's button all set) carried into the calls: while
// the app says the microphone is muted, every call's audio is muted in
// baresip too. baresip's mute sends silence, zeros encoded as usual, so RTP
// goes on (a PBX that waits for RTP, and a firewall that keeps the way in
// only while packets go out, see no difference); what a muted device still
// delivers (one tried left noise at -66 dBFS) goes no further.
//
// The app reads the mute off the endpoint the call records from with its
// looks at the volume (commands.rs) and says each change (ksip_audio_mute);
// it is not read here, so that no Windows call is made on baresip's thread
// under a call. What was said last is kept and put on every call, the ones
// that start later included.
//
// Threads: baresip's main thread only (the command, its timer). Lifetime:
// start() once the module is up, stop() before it goes (device_module.cpp).
#pragma once

namespace microphone_mute {
void start();
void stop();
// The app's word on the microphone: muted or not, from now on.
void set(bool muted);
} // namespace microphone_mute
